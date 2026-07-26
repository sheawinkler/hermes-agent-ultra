mod gateway_driver_tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};
    use tokio_tungstenite::accept_async;

    type MockDiscordWebSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    const IDENTIFY_PACING_TEST_TIMEOUT: Duration = Duration::from_secs(7);


    fn driver_test_config(
        gateway_url: String,
        bot_policy: DiscordBotMessagePolicy,
    ) -> DiscordConfig {
        DiscordConfig {
            token: "test-token".to_string(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            api_base_url: None,
            gateway_url: Some(gateway_url),
            liveness_interval_seconds: 0.0,
            liveness_failure_threshold: 1,
            require_mention: false,
            intents: 1 << 9,
            bot_policy,
            reply_to_mode: "first".to_string(),
            channel_controls: DiscordChannelControls::default(),
            channel_skill_bindings: Vec::new(),
        }
    }

    struct TestRuntime {
        runtime: Arc<DiscordGatewayRuntime>,
        _state_dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestRuntime {
        type Target = Arc<DiscordGatewayRuntime>;

        fn deref(&self) -> &Self::Target {
            &self.runtime
        }
    }

    fn test_runtime(
        gateway_url: String,
        bot_policy: DiscordBotMessagePolicy,
    ) -> TestRuntime {
        test_runtime_from_config(driver_test_config(gateway_url, bot_policy))
    }

    fn test_runtime_from_config(config: DiscordConfig) -> TestRuntime {
        test_runtime_from_config_and_api(
            config,
            Client::new(),
            "http://127.0.0.1:9".to_string(),
        )
    }

    fn test_runtime_from_config_and_api(
        config: DiscordConfig,
        rest_client: Client,
        api_base_url: String,
    ) -> TestRuntime {
        let state_dir = tempfile::tempdir().expect("isolated Discord test state");
        let tracker = DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("discord_threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        );
        let runtime = Arc::new(DiscordGatewayRuntime::new(
            &config,
            Arc::new(Mutex::new(tracker)),
            rest_client,
            api_base_url,
        ));
        runtime
            .thread_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_root("guild-1", "channel-1");
        TestRuntime {
            runtime,
            _state_dir: state_dir,
        }
    }

    async fn attach_test_durable_inbound(
        runtime: &DiscordGatewayRuntime,
        inbound_tx: mpsc::Sender<DiscordInboundMessage>,
    ) -> tokio::task::JoinHandle<()> {
        let (journal_tx, mut journal_rx) = mpsc::channel(16);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        tokio::spawn(async move {
            while let Some(event) = journal_rx.recv().await {
                if let DiscordIngressJournalEvent::Reserve(mut reservation) = event {
                    reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
                }
            }
        })
    }

    fn ordered_test_message(message_id: &str) -> IncomingDiscordMessage {
        IncomingDiscordMessage {
            channel_id: "channel-1".to_string(),
            channel_type: Some(0),
            guild_id: Some("guild-1".to_string()),
            parent_channel_id: None,
            thread_id: None,
            is_dm: false,
            message_id: message_id.to_string(),
            user_id: Some("user-1".to_string()),
            username: Some("Ada".to_string()),
            content: format!("payload-{message_id}"),
            is_bot: false,
            member_role_ids: vec!["role-1".to_string()],
            message_type: 0,
            mention_user_ids: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
            reply_to_attachments: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn message_create_before_ready_identity_is_not_enqueued() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let session = GatewaySession::new();
        let (ordered_tx, mut ordered_rx) = mpsc::channel(1);
        let outcome = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "pre-ready-message",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "hello",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(outcome, DiscordInboundDispatchOutcome::Ignored);
        assert!(
            timeout(Duration::from_millis(50), ordered_rx.recv())
                .await
                .is_err(),
            "pre-READY MESSAGE_CREATE must not reach ordered ingress"
        );
    }

    #[tokio::test]
    async fn blank_message_id_is_ignored_without_blocking_later_ordered_ingress() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (journal_ids_tx, journal_ids_rx) = tokio::sync::oneshot::channel();
        let journal_task = tokio::spawn(async move {
            let mut ids = Vec::new();
            while let Some(event) = journal_rx.recv().await {
                if let DiscordIngressJournalEvent::Reserve(mut reservation) = event {
                    ids.push(reservation.entry().message.message_id.clone());
                    reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
                }
            }
            let _ = journal_ids_tx.send(ids);
        });
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        let (sequence_ack_tx, _sequence_ack_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_ack_tx)
                .await;
        });
        let mut session = GatewaySession::new();
        session.client_user_id = Some("bot-self".to_string());

        let blank = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": " \t ",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "ignored",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(blank, DiscordInboundDispatchOutcome::Ignored);

        let valid = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "valid-after-blank",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "process me",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(valid, DiscordInboundDispatchOutcome::Queued);
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("valid ingress must not be blocked by blank predecessor")
            .expect("inbound receiver stays live");
        assert_eq!(inbound.message().message_id, "valid-after-blank");

        runtime.stop_tx.send_replace(true);
        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage exits cleanly")
            .expect("ordered stage task succeeds");
        drop(worker);
        drop(runtime);
        let journal_ids = timeout(Duration::from_secs(1), journal_ids_rx)
            .await
            .expect("journal sender closes on clean shutdown")
            .expect("journal task reports reserved IDs");
        assert_eq!(
            journal_ids,
            vec!["valid-after-blank"],
            "blank IDs must never enter the durable journal"
        );
        journal_task.await.expect("journal task succeeds");
    }

    #[tokio::test]
    async fn unsupported_interaction_commits_then_next_message_progresses_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(90, &url)).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::DISPATCH,
                    d: Some(serde_json::json!({
                        "id": "unsupported-interaction",
                        "type": 2,
                    })),
                    s: Some(91),
                    t: Some("INTERACTION_CREATE".to_string()),
                },
            )
            .await;
            send_payload(
                &mut socket,
                message_create_with_sequence("after-interaction", "human", false, None, 92),
            )
            .await;
            assert!(
                timeout(Duration::from_millis(250), socket.next()).await.is_err(),
                "unsupported interaction must not reconnect or replay before FIFO progresses"
            );
        });

        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("next MESSAGE_CREATE progresses")
            .expect("inbound receiver remains live");
        assert_eq!(inbound.message().message_id, "after-interaction");
        timeout(Duration::from_secs(1), server)
            .await
            .expect("mock gateway confirms no reconnect")
            .expect("mock gateway task succeeds");
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ready_message_racing_a_closed_ordered_stage_is_retained_for_shutdown_recovery() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        session.client_user_id = Some("bot-self".to_string());
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        drop(ordered_rx);
        runtime.stop_tx.send_replace(true);

        let outcome = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "ready-race-message",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "hello",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                Some(DiscordSequenceAck {
                    generation: 1,
                    sequence: 7,
                }),
                &ordered_tx,
            )
            .await;
        assert_eq!(outcome, DiscordInboundDispatchOutcome::Unhealthy);
        let retained = worker.take_ordered_overflow();
        assert_eq!(retained.len(), 1);
        let Some(DiscordOrderedIngress::Message {
            ack,
            ingress,
            journaled,
        }) = retained.front()
        else {
            panic!("closed ordered send must retain its returned message");
        };
        assert_eq!(ack.as_ref().map(|ack| ack.sequence), Some(7));
        assert_eq!(ingress.message.message_id, "ready-race-message");
        assert!(!journaled);
    }

    #[tokio::test]
    async fn durable_ingress_ack_gates_sequence_and_suppresses_replayed_reservations() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        let (sequence_tx, mut sequence_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });

        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: Some(DiscordSequenceAck {
                    generation: 7,
                    sequence: 41,
                }),
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-41"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        let mut reservation = match timeout(Duration::from_secs(1), journal_rx.recv())
            .await
            .expect("durability reservation")
            .expect("journal channel open")
        {
            DiscordIngressJournalEvent::Reserve(reservation) => reservation,
            DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
        };
        assert_eq!(reservation.entry().message().message_id, "message-41");
        assert!(matches!(
            inbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            sequence_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
        assert_eq!(
            timeout(Duration::from_secs(1), sequence_rx.recv())
                .await
                .expect("sequence commit after durability ACK"),
            Some(DiscordSequenceAck {
                generation: 7,
                sequence: 41,
            })
        );
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("routable ingress after durability ACK")
            .expect("inbound channel open");
        assert_eq!(inbound.message().message_id, "message-41");

        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: Some(DiscordSequenceAck {
                    generation: 7,
                    sequence: 42,
                }),
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-41"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        let mut replay = match timeout(Duration::from_secs(1), journal_rx.recv())
            .await
            .expect("replay durability reservation")
            .expect("journal channel open")
        {
            DiscordIngressJournalEvent::Reserve(reservation) => reservation,
            DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
        };
        replay.acknowledge_durable(DiscordIngressReservationOutcome::ExistingRouteOwned);
        assert_eq!(
            timeout(Duration::from_secs(1), sequence_rx.recv())
                .await
                .expect("replayed sequence commit"),
            Some(DiscordSequenceAck {
                generation: 7,
                sequence: 42,
            })
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            inbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        runtime
            .queue_recovered_pre_policy_ingress(vec![DiscordJournalEntry::new(
                ordered_test_message("message-recovered"),
                Some("bot-self".to_string()),
            )])
            .expect("pre-policy recovery is accepted");
        let recovered = worker.take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (recovered, journaled) = recovered.into_iter().next().unwrap();
        assert!(journaled);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .unwrap();
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("recovered ingress bypasses re-reservation")
            .expect("inbound channel open");
        assert_eq!(inbound.message().message_id, "message-recovered");
        assert!(matches!(
            journal_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage stops")
            .expect("ordered stage joins");
    }

    #[tokio::test]
    async fn stop_durably_handoffs_unjournaled_ordered_ingress_before_restart() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-unfsynced"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        runtime.stop_tx.send_replace(true);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let draining = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });
        timeout(Duration::from_secs(1), draining)
            .await
            .expect("stopped ordered stage drains into shared recovery")
            .expect("ordered stage joins");
        let flush = runtime.flush_recovered_ingress_for_shutdown();
        let journal = async {
            let mut reservation = match timeout(Duration::from_secs(1), journal_rx.recv())
                .await
                .expect("shutdown durability reservation")
                .expect("journal channel open")
            {
                DiscordIngressJournalEvent::Reserve(reservation) => reservation,
                DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
            };
            assert_eq!(reservation.entry().message().message_id, "message-unfsynced");
            reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
        };
        let (flush, ()) = tokio::join!(flush, journal);
        flush.expect("shutdown durability handoff");

        let recovered = runtime.worker().take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (recovered, journaled) = recovered.into_iter().next().unwrap();
        assert!(journaled, "shutdown handoff must commit only after journal ACK");

        runtime.stop_tx.send_replace(false);
        let (replay_tx, replay_rx) = mpsc::channel(4);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let replay_worker = runtime.worker();
        let stage = tokio::spawn(async move {
            replay_worker
                .run_ordered_ingress_stage(replay_rx, sequence_tx)
                .await;
        });
        replay_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), inbound_rx.recv())
                .await
                .expect("durably handed-off ingress is routed after restart")
                .expect("inbound channel open")
                .message()
                .message_id,
            "message-unfsynced"
        );
        assert!(matches!(
            journal_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(replay_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("replay stage stops")
            .expect("replay stage joins");
    }

    #[tokio::test]
    async fn cancelled_shutdown_handoff_keeps_full_journal_entries_recoverable_in_fifo_order() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        journal_tx
            .send(DiscordIngressJournalEvent::Remove {
                message_id: "already-full".to_string(),
            })
            .await
            .expect("fill journal channel");
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        worker.requeue_recovered_ingress(vec![
            (
                DiscordJournalEntry::new(
                    ordered_test_message("message-cancel-first"),
                    Some("bot-self".to_string()),
                ),
                false,
            ),
            (
                DiscordJournalEntry::new(
                    ordered_test_message("message-cancel-second"),
                    Some("bot-self".to_string()),
                ),
                false,
            ),
        ]);

        assert!(
            timeout(
                Duration::from_millis(50),
                runtime.flush_recovered_ingress_for_shutdown()
            )
            .await
            .is_err(),
            "a full journal must leave shutdown handoff blocked until cancellation"
        );

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["message-cancel-first", "message-cancel-second"],
            "cancellation must retain every unjournaled entry in FIFO order"
        );
        assert!(
            recovered.iter().all(|(_, journaled)| !journaled),
            "cancelled reservations must not claim durable ownership"
        );
    }

    #[tokio::test]
    async fn aborting_blocked_ordered_stage_recovers_current_and_receiver_backlog() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        journal_tx
            .send(DiscordIngressJournalEvent::Remove {
                message_id: "already-full".to_string(),
            })
            .await
            .expect("fill journal channel");
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        for message_id in ["message-stage-first", "message-stage-second"] {
            ordered_tx
                .send(DiscordOrderedIngress::Message {
                    ack: None,
                    ingress: DiscordUnresolvedIngress {
                        message: ordered_test_message(message_id),
                        client_user_id: Some("bot-self".to_string()),
                    },
                    journaled: false,
                })
                .await
                .expect("queue ordered ingress");
        }
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let mut stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });
        assert!(
            timeout(Duration::from_millis(50), &mut stage).await.is_err(),
            "the first reservation must block on the full journal"
        );
        stage.abort();
        assert!(stage.await.expect_err("aborted stage must cancel").is_cancelled());

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["message-stage-first", "message-stage-second"],
            "abort must retain the in-flight entry and ordered receiver backlog"
        );
        assert!(recovered.iter().all(|(_, journaled)| !journaled));
    }

    #[tokio::test]
    async fn forbidden_thread_lookup_rejects_only_its_head_and_advances_later_ingress() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind forbidden lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept lookup request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read lookup request");
            socket
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .expect("write forbidden lookup response");
        });

        let config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });

        let mut forbidden = ordered_test_message("forbidden-head");
        forbidden.channel_id = "thread-without-view-channel".to_string();
        forbidden.channel_type = Some(11);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: forbidden,
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue forbidden head");
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("after-forbidden"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue later valid message");

        let routed = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("later ingress progresses without reconnecting")
            .expect("inbound channel remains live");
        assert_eq!(routed.message().message_id, "after-forbidden");
        assert!(
            runtime
                .snapshot_recovered_ingress_for_cli()
                .iter()
                .all(|entry| entry.message().message_id != "forbidden-head"),
            "conclusive 403 must remove the durable rejected head"
        );

        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage exits after its sender closes")
            .expect("ordered stage succeeds");
        server.await.expect("lookup fixture succeeds");
    }

    #[tokio::test]
    async fn stalled_channel_get_body_preserves_fifo_for_recovery() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stalled lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stalled, _) = listener.accept().await.expect("accept stalled lookup");
            let mut request = [0_u8; 4096];
            let _ = stalled.read(&mut request).await.expect("read stalled lookup");
            stalled
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 512\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write incomplete lookup headers");

            // The lookup deadline must abandon this incomplete successful
            // response instead of retrying around its durable FIFO head.
            let _ = timeout(Duration::from_secs(7), stalled.read(&mut request)).await;
        });

        let config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });

        let mut stalled = ordered_test_message("stalled-head");
        stalled.channel_id = "thread-stalled".to_string();
        stalled.channel_type = Some(11);
        for message in [stalled, ordered_test_message("after-stalled")] {
            ordered_tx
                .send(DiscordOrderedIngress::Message {
                    ack: None,
                    ingress: DiscordUnresolvedIngress {
                        message,
                        client_user_id: Some("bot-self".to_string()),
                    },
                    journaled: false,
                })
                .await
                .expect("queue ordered ingress");
        }
        drop(ordered_tx);

        timeout(Duration::from_secs(7), stage)
            .await
            .expect("incomplete successful lookup stops the ordered stage")
            .expect("ordered stage succeeds");
        assert!(
            matches!(
                inbound_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "the stalled head must not route and later ingress must not advance around it"
        );
        assert_eq!(
            runtime
                .snapshot_recovered_ingress_for_cli()
                .iter()
                .map(|entry| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["stalled-head", "after-stalled"],
            "the incomplete 2xx head and its FIFO backlog must remain recoverable"
        );
        server.await.expect("lookup fixture succeeds");
    }

    #[tokio::test]
    async fn runtime_abort_joins_ordered_child_before_immediate_recovery_snapshot() {
        use tokio::io::AsyncReadExt;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind blocked lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let (lookup_started_tx, lookup_started_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("lookup request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read lookup request");
            let _ = lookup_started_tx.send(());
            std::future::pending::<()>().await;
        });
        let gateway_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind authenticated gateway fixture");
        let gateway_url = format!("ws://{}", gateway_listener.local_addr().unwrap());
        let gateway_resume_url = gateway_url.clone();
        let gateway_server = tokio::spawn(async move {
            let mut socket = accept(&gateway_listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(1, &gateway_resume_url)).await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime abort closes the authenticated gateway socket");
        });


        let config = driver_test_config(gateway_url, DiscordBotMessagePolicy::None);
        let runtime =
            test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let mut current = ordered_test_message("abort-current");
        current.channel_id = "thread-blocked".to_string();
        current.channel_type = Some(11);
        let mut backlog = current.clone();
        backlog.message_id = "abort-backlog".to_string();
        worker.requeue_recovered_ingress([
            (
                DiscordJournalEntry::new(current, Some("bot-self".to_string())),
                true,
            ),
            (
                DiscordJournalEntry::new(backlog, Some("bot-self".to_string())),
                true,
            ),
        ]);

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(1), lookup_started_rx)
            .await
            .expect("first ordered ingress reaches blocked REST lookup")
            .expect("lookup fixture signals request");

        runtime.abort().await;
        let snapshot = runtime.snapshot_recovered_ingress_for_cli();
        assert_eq!(
            snapshot
                .iter()
                .map(|entry| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["abort-current", "abort-backlog"],
            "awaited runtime abort must join the child before recovery is snapshotted"
        );

        gateway_server
            .await
            .expect("authenticated gateway fixture stops after runtime abort");

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn saturated_stop_restart_retains_authenticated_mention_identity() {
        let mut config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.require_mention = true;
        let runtime = test_runtime_from_config(config);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        inbound_tx
            .send(test_inbound_message("already-queued"))
            .await
            .expect("fill CLI-owned inbound queue");
        let (journal_tx, _journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let mut mentioned = ordered_test_message("mentioned-before-stop");
        mentioned.mention_user_ids = vec!["bot-self".to_string()];
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: mentioned,
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: true,
            })
            .await
            .expect("queue journal-owned mentioned ingress");
        let draining_worker = worker.clone();
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let draining = tokio::spawn(async move {
            draining_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });

        // Give the resolver one bounded enqueue attempt against the full queue,
        // then interrupt its retry. The requeue originates from
        // `DiscordInboundMessage::journal_entry`.
        tokio::time::sleep(Duration::from_millis(DISCORD_INBOUND_RETRY_MS * 2)).await;
        runtime.stop_tx.send_replace(true);
        drop(ordered_tx);
        timeout(Duration::from_secs(1), draining)
            .await
            .expect("full-queue stop interrupts ordered ingress")
            .expect("ordered ingress joins");

        let recovered = worker.take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (entry, journaled) = recovered.into_iter().next().expect("recovered entry");
        assert!(journaled);
        assert_eq!(entry.client_user_id(), Some("bot-self"));

        runtime.stop_tx.send_replace(false);
        assert_eq!(
            inbound_rx
                .recv()
                .await
                .expect("preexisting CLI ingress remains queued")
                .message()
                .message_id,
            "already-queued"
        );
        runtime
            .queue_recovered_pre_policy_ingress(vec![entry])
            .expect("preserved pre-policy entry requeues");
        let (recovered, journaled) = worker
            .take_recovered_ingress()
            .into_iter()
            .next()
            .expect("restart receives the preserved record");
        assert!(journaled);
        let (replay_tx, replay_rx) = mpsc::channel(1);
        replay_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .expect("replay preserved journal entry");
        drop(replay_tx);
        let replay_worker = runtime.worker();
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let replay = tokio::spawn(async move {
            replay_worker.run_ordered_ingress_stage(replay_rx, sequence_tx).await;
        });
        assert_eq!(
            timeout(Duration::from_secs(1), inbound_rx.recv())
                .await
                .expect("replayed mentioned message is routed")
                .expect("inbound sender remains open")
                .message()
                .message_id,
            "mentioned-before-stop"
        );
        timeout(Duration::from_secs(1), replay)
            .await
            .expect("replay stage stops")
            .expect("replay stage joins");
    }

    #[tokio::test]
    async fn ordered_overflow_is_appended_after_queued_ingress_on_shutdown() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-a"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue first ingress");
        worker.retain_ordered_overflow(DiscordOrderedIngress::Message {
            ack: None,
            ingress: DiscordUnresolvedIngress {
                message: ordered_test_message("message-b"),
                client_user_id: Some("bot-self".to_string()),
            },
            journaled: false,
        });

        runtime.stop_tx.send_replace(true);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        worker.requeue_ordered_overflow(worker.take_ordered_overflow());

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["message-a", "message-b"],
            "a full-stage overflow must retain every earlier queued ingress first"
        );
        assert!(
            recovered.iter().all(|(_, journaled)| !journaled),
            "neither item may claim durable ownership before the ordered handoff"
        );
    }

    #[test]
    fn ipv6_gateway_host_is_normalized_and_bracketed_exactly_once_for_connect() {
        let url = url::Url::parse("wss://[::1]:443").unwrap();
        let host = discord_url_host(&url).expect("IPv6 host");
        assert_eq!(host, "::1");
        assert_eq!(discord_connect_authority(&host, 443), "[::1]:443");
    }

    #[test]
    fn gateway_url_preserves_semantic_queries_and_rejects_compression() {
        let normalized = discord_gateway_v10_url(
            "wss://gateway.example/private?tenant=acme&routing=blue&v=9&encoding=etf",
        )
        .expect("private semantic gateway URL is supported");
        let parsed = url::Url::parse(&normalized).unwrap();
        assert_eq!(
            parsed.query_pairs().collect::<Vec<_>>(),
            vec![
                ("tenant".into(), "acme".into()),
                ("routing".into(), "blue".into()),
                ("v".into(), "10".into()),
                ("encoding".into(), "json".into()),
            ]
        );
        assert!(
            discord_gateway_v10_url("wss://gateway.discord.gg/?compress=zlib-stream").is_err(),
            "the text-frame driver must fail closed instead of advertising compression"
        );
        assert!(discord_gateway_v10_url("wss://user:secret@gateway.discord.gg").is_err());
        assert!(discord_gateway_v10_url("wss://gateway.discord.gg/#fragment").is_err());
    }

    #[tokio::test]
    async fn compressed_gateway_config_fails_before_opening_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = driver_test_config(
            format!(
                "ws://{}/?tenant=acme&compress=zlib-stream",
                listener.local_addr().unwrap()
            ),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config(config);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let error = runtime
            .start()
            .await
            .expect_err("unsupported compression must fail before worker spawn");
        assert!(error.to_string().contains("compression is unsupported"));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "rejected compression must not connect and loop on binary frames"
        );
    }

    #[tokio::test]
    async fn fragmented_gateway_config_fails_before_opening_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = driver_test_config(
            format!("ws://{}/?tenant=acme#not-for-websocket", listener.local_addr().unwrap()),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config(config);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let error = runtime
            .start()
            .await
            .expect_err("fragmented gateway config must fail before worker spawn");
        assert!(error.to_string().contains("must not include a fragment"));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "a rejected fragment must not cause repeated websocket handshakes"
        );
    }

    #[tokio::test]
    async fn runtime_start_requires_live_inbound_and_durable_journal_receivers() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );

        let error = runtime
            .start()
            .await
            .expect_err("a memory-only runtime must not start");
        assert!(error
            .to_string()
            .contains("durable ingress journal receiver is unavailable"));
        assert!(runtime.task.lock().await.is_none());
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));

        let (journal_tx, _journal_rx) = mpsc::channel(1);
        *runtime.journal_tx.write().await = Some(journal_tx);
        let error = runtime
            .start()
            .await
            .expect_err("a journal without an inbound receiver must not start");
        assert!(error
            .to_string()
            .contains("Gateway inbound receiver is unavailable"));
        assert!(runtime.task.lock().await.is_none());
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_runtime_aborts_its_socket_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            let _ = accepted_tx.send(());
            let _ = socket.next().await;
        });

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("worker opens its socket")
            .expect("server signals accepted socket");
        let abort = runtime
            .task
            .lock()
            .await
            .as_ref()
            .expect("runtime owns worker task")
            .abort_handle();

        drop(runtime);
        timeout(Duration::from_secs(1), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping runtime aborts the socket worker");
        timeout(Duration::from_secs(1), server)
            .await
            .expect("server observes worker socket close")
            .expect("server joins");
    }

    #[tokio::test]
    async fn runtime_start_rejects_invalid_gateway_override_before_spawning_worker() {
        let runtime = test_runtime(
            "https://gateway.discord.test/not-a-websocket".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let error = runtime.start().await.expect_err("invalid URL must fail start");
        assert!(matches!(error, GatewayError::ConnectionFailed(_)));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(runtime.task.lock().await.is_none());
    }

    #[test]
    fn stale_resume_url_is_ignored_without_resumable_session_state() {
        let runtime = test_runtime(
            "ws://primary.example.test".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        session.resume_gateway_url = Some("ws://stale.example.test".to_string());
        session.session_id = Some("session-1".to_string());

        let primary = url::Url::parse(&worker.connection_url(&session).unwrap()).unwrap();
        assert_eq!(primary.host_str(), Some("primary.example.test"));

        session.commit_sequence(Some(41));
        let resumable = url::Url::parse(&worker.connection_url(&session).unwrap()).unwrap();
        assert_eq!(resumable.host_str(), Some("stale.example.test"));
    }

    #[tokio::test]
    async fn http_connect_proxy_tunnels_gateway_bytes_with_basic_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let mut config = driver_test_config(
            "ws://gateway.example.test:8080".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.proxy.http_proxy =
            Some(format!("http://agent:secret@{proxy_address}"));
        let runtime = test_runtime_from_config(config);

        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 16 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(
                "CONNECT gateway.example.test:8080 HTTP/1.1\r\n"
            ));
            let credentials = base64::engine::general_purpose::STANDARD
                .encode("agent:secret");
            assert!(request.contains(&format!(
                "\r\nProxy-Authorization: Basic {credentials}\r\n"
            )));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut ping = [0_u8; 4];
            stream.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let url = url::Url::parse("ws://gateway.example.test:8080").unwrap();
        let mut transport = runtime
            .worker()
            .open_transport(&url)
            .await
            .expect("HTTP CONNECT tunnel");
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn socks5h_proxy_tunnels_gateway_bytes_with_username_and_password() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let mut config = driver_test_config(
            "ws://gateway.example.test:443".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.proxy.socks_proxy =
            Some(format!("socks5h://agent:p%40ss@{proxy_address}"));
        let runtime = test_runtime_from_config(config);

        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0_u8; 2];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0_u8; usize::from(greeting[1])];
            stream.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&2));
            stream.write_all(&[5, 2]).await.unwrap();

            let mut auth = [0_u8; 2];
            stream.read_exact(&mut auth).await.unwrap();
            assert_eq!(auth[0], 1);
            let mut username = vec![0_u8; usize::from(auth[1])];
            stream.read_exact(&mut username).await.unwrap();
            let mut password_len = [0_u8; 1];
            stream.read_exact(&mut password_len).await.unwrap();
            let mut password = vec![0_u8; usize::from(password_len[0])];
            stream.read_exact(&mut password).await.unwrap();
            assert_eq!(&username, b"agent");
            assert_eq!(&password, b"p@ss");
            stream.write_all(&[1, 0]).await.unwrap();

            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [5, 1, 0, 3]);
            let mut domain_len = [0_u8; 1];
            stream.read_exact(&mut domain_len).await.unwrap();
            let mut domain = vec![0_u8; usize::from(domain_len[0])];
            stream.read_exact(&mut domain).await.unwrap();
            let mut port = [0_u8; 2];
            stream.read_exact(&mut port).await.unwrap();
            assert_eq!(&domain, b"gateway.example.test");
            assert_eq!(u16::from_be_bytes(port), 443);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();

            let mut ping = [0_u8; 4];
            stream.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let url = url::Url::parse("ws://gateway.example.test:443").unwrap();
        let mut transport = runtime
            .worker()
            .open_transport(&url)
            .await
            .expect("SOCKS5h tunnel");
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn locally_resolved_socks5_retries_each_target_under_one_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let first = "127.0.0.1:41001".parse::<std::net::SocketAddr>().unwrap();
        let second = "127.0.0.2:41002".parse::<std::net::SocketAddr>().unwrap();
        let proxy = tokio::spawn(async move {
            for (expected, reply_code) in [(first, 5_u8), (second, 0_u8)] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 2];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1]);
                let mut method = [0_u8; 1];
                stream.read_exact(&mut method).await.unwrap();
                assert_eq!(method, [0]);
                stream.write_all(&[5, 0]).await.unwrap();

                let mut request = [0_u8; 10];
                stream.read_exact(&mut request).await.unwrap();
                assert_eq!(request[..4], [5, 1, 0, 1]);
                assert_eq!(
                    std::net::SocketAddr::from((
                        std::net::Ipv4Addr::new(request[4], request[5], request[6], request[7]),
                        u16::from_be_bytes([request[8], request[9]]),
                    )),
                    expected
                );
                if reply_code != 0 {
                    // Deliberately blackhole the first CONNECT response. The
                    // client must reserve time for the second DNS address
                    // instead of letting this attempt consume the outer limit.
                    let mut eof = [0_u8; 1];
                    let read = timeout(Duration::from_millis(750), stream.read(&mut eof))
                        .await
                        .expect("first SOCKS attempt must be cancelled at its slice");
                    assert_eq!(read.unwrap(), 0);
                    continue;
                }
                stream
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let mut ping = [0_u8; 4];
                stream.read_exact(&mut ping).await.unwrap();
                assert_eq!(&ping, b"ping");
                stream.write_all(b"pong").await.unwrap();
            }
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut transport = DiscordGatewayWorker::connect_socks5_targets(
            &proxy_address.ip().to_string(),
            proxy_address.port(),
            vec![first, second],
            "",
            "",
            deadline,
        )
        .await
        .expect("the second local DNS target must be attempted");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the successful fallback must retain the shared outer deadline"
        );
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[test]
    fn private_gateway_realms_scope_tenant_credentials_without_leaking_transport_knobs() {
        let mut tenant_a = driver_test_config(
            "wss://alice:secret@gateway.example/private?tenant=A&v=9&encoding=etf"
                .to_string(),
            DiscordBotMessagePolicy::None,
        );
        let mut tenant_b = tenant_a.clone();
        tenant_b.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?encoding=json&tenant=B&v=10"
                .to_string(),
        );
        let mut other_credentials = tenant_a.clone();
        other_credentials.gateway_url = Some(
            "wss://bob:secret@gateway.example/private?tenant=A&v=10&encoding=json"
                .to_string(),
        );
        let mut transport_equivalent = tenant_a.clone();
        transport_equivalent.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?encoding=json&v=10&tenant=A"
                .to_string(),
        );
        let mut rejected_compression = tenant_a.clone();
        rejected_compression.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?tenant=A&compress=zlib-stream"
                .to_string(),
        );

        let tenant_a_scope = DiscordAdapter::state_scope_for_config(&tenant_a);
        assert_ne!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&tenant_b),
            "semantic tenant query must isolate persisted state"
        );
        assert_ne!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&other_credentials),
            "private userinfo must isolate persisted state"
        );
        assert_eq!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&transport_equivalent),
            "gateway v/encoding and query order are transport-only"
        );
        assert_eq!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&rejected_compression),
            "rejected compress remains a transport-only knob for realm identity"
        );
    }

    #[test]
    fn identify_pacer_spaces_fresh_identifies_for_one_shard() {
        let mut pacer = DiscordIdentifyPacer::default();
        let first = pacer.next_deadline();
        assert!(pacer.reserve_if_ready());
        let second = pacer.next_deadline();
        assert!(
            second.duration_since(first)
                >= Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS)
        );
    }

    #[test]
    fn identify_pacer_rearms_exhausted_daily_budget() {
        let mut pacer = DiscordIdentifyPacer::default();
        pacer.configure(DiscordGatewaySessionStartLimit {
            total: 2,
            remaining: 0,
            reset_after_ms: 60_000,
        });
        let reset_at = pacer.reset_at.expect("configured reset");
        let blocked = pacer.next_deadline();
        assert_eq!(blocked, reset_at);
        assert!(!pacer.reserve_if_ready());

        pacer.reset_at = Some(tokio::time::Instant::now() - Duration::from_millis(1));
        let refreshed = pacer.next_deadline();
        assert_eq!(pacer.remaining, Some(2));
        assert!(
            pacer
                .reset_at
                .expect("rearmed reset")
                .duration_since(tokio::time::Instant::now())
                >= Duration::from_secs(DISCORD_IDENTIFY_BUDGET_WINDOW_SECS)
                    - Duration::from_millis(1)
        );
        assert!(refreshed <= tokio::time::Instant::now());
        assert!(pacer.reserve_if_ready());
        assert_eq!(pacer.remaining, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn requested_heartbeat_preserves_scheduled_window() {
        let mut session = GatewaySession::new();
        session.heartbeat_interval_ms = Some(1_000);
        let original = DiscordGatewayWorker::next_heartbeat_deadline(&session)
            .expect("scheduled heartbeat");

        tokio::time::advance(Duration::from_millis(999)).await;
        session.heartbeat_sent();
        let unchanged = DiscordGatewayWorker::heartbeat_deadline_after_payload(
            Some(original),
            &session,
            opcodes::HEARTBEAT,
        )
        .expect("periodic heartbeat schedule remains active");

        assert_eq!(unchanged, original);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(tokio::time::Instant::now(), unchanged);
        assert_eq!(session.heartbeat_tick(), HeartbeatTick::Reconnect);
    }

    #[tokio::test(start_paused = true)]
    async fn initial_heartbeat_deadline_uses_injected_jitter() {
        let mut session = GatewaySession::new();
        session.heartbeat_interval_ms = Some(1_000);

        let now = tokio::time::Instant::now();

        let quarter = DiscordGatewayWorker::initial_heartbeat_deadline(&session, 0.25)
            .expect("jittered initial heartbeat");
        assert_eq!(quarter.duration_since(now), Duration::from_millis(250));

        let clamped = DiscordGatewayWorker::initial_heartbeat_deadline(&session, 2.0)
            .expect("clamped jittered initial heartbeat");
        assert_eq!(clamped.duration_since(now), Duration::from_secs(1));
    }

    #[test]
    fn invalid_session_reconnect_paces_both_resumable_modes() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime
            .worker()
            .with_invalid_session_delay(|| Duration::from_millis(1_234));

        assert_eq!(
            worker.invalid_session_reconnect_outcome(true),
            DiscordGatewayConnectionOutcome::ReconnectAfter(Duration::from_millis(1_234))
        );
        assert_eq!(
            worker.invalid_session_reconnect_outcome(false),
            DiscordGatewayConnectionOutcome::ReconnectAfter(Duration::from_millis(1_234))
        );
    }

    #[tokio::test]
    async fn exhausted_identify_wait_observes_an_already_stopped_runtime() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        runtime
            .configure_identify_session_start_limit(DiscordGatewaySessionStartLimit {
                total: 0,
                remaining: 0,
                reset_after_ms: 60_000,
            })
            .await;
        runtime.stop_tx.send_replace(true);

        let mut stop = runtime.stop_tx.subscribe();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let result = timeout(
            Duration::from_millis(100),
            runtime
                .worker()
                .await_identify_eligibility(&mut stop, &mut ingress_done),
        )
        .await
        .expect("already-stopped paced wait returns promptly");
        assert!(matches!(
            result,
            DiscordAuthenticationPreparation::Stopped
        ));
    }

    async fn accept(listener: &TcpListener) -> MockDiscordWebSocket {
        let (stream, _) = listener.accept().await.expect("mock gateway accepts TCP");
        accept_async(stream).await.expect("mock gateway accepts WebSocket")
    }

    async fn send_payload(socket: &mut MockDiscordWebSocket, payload: GatewayPayload) {
        socket
            .send(Message::Text(
                serde_json::to_string(&payload)
                    .expect("mock payload serializes")
                    .into(),
            ))
            .await
            .expect("mock gateway sends payload");
    }

    async fn receive_opcode(
        socket: &mut MockDiscordWebSocket,
        opcode: u8,
    ) -> GatewayPayload {
        receive_opcode_with_timeout(socket, opcode, Duration::from_secs(1)).await
    }

    async fn receive_opcode_with_timeout(
        socket: &mut MockDiscordWebSocket,
        opcode: u8,
        wait_for_payload: Duration,
    ) -> GatewayPayload {
        for _ in 0..4 {
            let frame = timeout(wait_for_payload, socket.next())
                .await
                .expect("client emits a gateway payload")
                .expect("client socket remains open")
                .expect("client gateway frame succeeds");
            if let Message::Text(text) = frame {
                let payload = serde_json::from_str::<GatewayPayload>(text.as_str())
                    .expect("client payload is JSON");
                if payload.op == opcode {
                    return payload;
                }
            }
        }
        panic!("client did not emit opcode {opcode}");
    }

    fn hello(interval_ms: u64) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": interval_ms })),
            s: None,
            t: None,
        }
    }

    fn ready(sequence: u64, resume_gateway_url: &str) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "session_id": "session-1",
                "resume_gateway_url": resume_gateway_url,
                "user": { "id": "self-id", "username": "Hermes" }
            })),
            s: Some(sequence),
            t: Some("READY".to_string()),
        }
    }

    #[tokio::test]
    async fn durability_ack_arriving_during_connection_preparation_forces_resume() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        let actions = session.handle_gateway_event(&ready(42, "ws://resume.example"));
        assert!(matches!(
            actions.as_slice(),
            [GatewayAction::Dispatch(name, _)] if name == "READY"
        ));
        assert!(!session.can_resume());

        let ack = DiscordSequenceAck {
            generation: session.generation(),
            sequence: 42,
        };
        let mut pending_initial_ack = None;
        DiscordGatewayWorker::track_initial_durability(
            &mut pending_initial_ack,
            true,
            Some(ack),
            DiscordInboundDispatchOutcome::Queued,
        );
        let (ack_tx, mut ack_rx) = mpsc::unbounded_channel();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let (_stop_tx, mut stop) = watch::channel(false);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            ack_tx.send(ack).unwrap();
        });

        let preparation = timeout(
            Duration::from_secs(1),
            worker.await_authentication_preparation(
                &mut session,
                &mut ack_rx,
                &mut pending_initial_ack,
                &mut ingress_done,
                &mut stop,
            ),
        )
        .await
        .expect("durability ACK must interrupt connection preparation")
        .unwrap();
        assert_eq!(preparation, DiscordAuthenticationPreparation::Ready);
        assert!(pending_initial_ack.is_none());
        assert!(session.can_resume());
        assert_eq!(
            session.handle_gateway_event(&hello(41_250)),
            vec![GatewayAction::SendResume]
        );
    }

    #[tokio::test]
    async fn ready_backpressure_does_not_wait_for_an_unenqueued_ack() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        let _ = session.handle_gateway_event(&ready(42, "ws://resume.example"));
        let ack = DiscordSequenceAck {
            generation: session.generation(),
            sequence: 42,
        };
        let mut pending_initial_ack = Some(ack);
        DiscordGatewayWorker::track_initial_durability(
            &mut pending_initial_ack,
            true,
            Some(ack),
            DiscordInboundDispatchOutcome::Backpressure,
        );
        assert!(pending_initial_ack.is_none());

        let (_ack_tx, mut ack_rx) = mpsc::unbounded_channel();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let (_stop_tx, mut stop) = watch::channel(false);
        let preparation = timeout(
            Duration::from_secs(1),
            worker.await_authentication_preparation(
                &mut session,
                &mut ack_rx,
                &mut pending_initial_ack,
                &mut ingress_done,
                &mut stop,
            ),
        )
        .await
        .expect("backpressured READY must reconnect for fresh IDENTIFY")
        .unwrap();
        assert_eq!(preparation, DiscordAuthenticationPreparation::Ready);
    }

    fn message_create(
        id: &str,
        author_id: &str,
        is_bot: bool,
        guild_id: Option<&str>,
    ) -> GatewayPayload {
        let mut data = serde_json::json!({
            "id": id,
            "channel_id": "channel-1",
            "channel_type": if guild_id.is_some() { 0 } else { 1 },
            "content": "hello",
            "author": { "id": author_id, "username": "sender", "bot": is_bot }
        });
        if let Some(guild_id) = guild_id {
            data["guild_id"] = serde_json::Value::String(guild_id.to_string());
        }
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(data),
            s: Some(3),
            t: Some("MESSAGE_CREATE".to_string()),
        }
    }

    fn thread_create(thread_id: &str, parent_channel_id: &str) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "id": thread_id,
                "guild_id": "guild-1",
                "parent_id": parent_channel_id,
                "type": 11,
            })),
            s: Some(3),
            t: Some("THREAD_CREATE".to_string()),
        }
    }

    fn message_create_with_sequence(
        id: &str,
        author_id: &str,
        is_bot: bool,
        guild_id: Option<&str>,
        sequence: u64,
    ) -> GatewayPayload {
        let mut payload = message_create(id, author_id, is_bot, guild_id);
        payload.s = Some(sequence);
        payload
    }

    fn test_inbound_message(id: &str) -> DiscordInboundMessage {
        let data = message_create(id, "filler-author", false, None)
            .d
            .expect("test message has payload data");
        let message =
            DiscordAdapter::parse_message_create(&data).expect("test message parses as inbound");
        DiscordInboundMessage {
            sender_is_bot: message.is_bot,
            client_user_id: Some("bot-self".to_string()),
            message,
        }
    }

    #[test]
    fn thread_context_cache_enriches_standard_messages_and_fails_closed_on_a_known_miss() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        worker.observe_thread_context(
            "GUILD_CREATE",
            &serde_json::json!({
                "id": "guild-a",
                "threads": [{ "id": "thread-a", "parent_id": "parent-a", "type": 11 }]
            }),
        );
        worker.observe_thread_context(
            "THREAD_LIST_SYNC",
            &serde_json::json!({
                "guild_id": "guild-a",
                "threads": [{ "id": "thread-b", "parent_id": "parent-b", "type": 11 }]
            }),
        );

        let mut standard = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-a",
            "channel_id": "thread-a",
            "guild_id": "guild-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("standard message parses");
        assert!(standard.thread_id.is_none());
        assert_eq!(
            worker.resolve_thread_parent(&mut standard),
            DiscordThreadResolution::Resolved
        );
        assert_eq!(standard.parent_channel_id.as_deref(), Some("parent-a"));
        assert_eq!(standard.thread_id.as_deref(), Some("thread-a"));

        let mut cross_guild = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-cross-guild",
            "channel_id": "thread-a",
            "guild_id": "guild-b",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("cross-guild message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut cross_guild),
            DiscordThreadResolution::Rejected
        );

        let mut missing_guild = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-missing-guild",
            "channel_id": "thread-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("missing-guild message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut missing_guild),
            DiscordThreadResolution::Rejected
        );

        assert!(
            !missing_guild.is_dm,
            "missing guild/channel metadata must not default into the DM policy path"
        );

        let mut explicit_dm = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "explicit-dm",
            "channel_id": "dm-channel",
            "channel_type": 1,
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("explicit DM parses");
        assert!(explicit_dm.is_dm);
        assert_eq!(
            worker.resolve_thread_parent(&mut explicit_dm),
            DiscordThreadResolution::Resolved
        );
        let mut unresolvable = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-unknown",
            "channel_id": "unknown-thread",
            "channel_type": 11,
            "guild_id": "guild-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("known thread message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut unresolvable),
            DiscordThreadResolution::NeedsLookup
        );
    }

    #[tokio::test]
    async fn routed_thread_ingress_does_not_count_as_hermes_participation() {
        let mut config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.require_mention = true;
        let runtime = test_runtime_from_config(config);
        let worker = runtime.worker();
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let mut mentioned = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "mentioned",
            "channel_id": "thread-unanswered",
            "guild_id": "guild-a",
            "content": "<@self-id> please answer",
            "mentions": [{ "id": "self-id", "username": "hermes", "bot": true }],
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("mentioned thread message parses");
        mentioned.thread_id = Some("thread-unanswered".to_string());
        mentioned.parent_channel_id = Some("parent-a".to_string());
        let inbound = worker
            .prepare_routable_inbound(mentioned.clone(), Some("self-id"))
            .expect("mention is eligible for routing");
        assert!(matches!(
            worker.try_enqueue_inbound(inbound).await,
            DiscordInboundQueueOutcome::Queued
        ));
        assert_eq!(
            rx.recv().await.expect("routed ingress").message().message_id,
            "mentioned"
        );
        assert!(!runtime
            .thread_participation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains("thread-unanswered"));

        let mut follow_up = mentioned;
        follow_up.message_id = "unmentioned-follow-up".to_string();
        follow_up.mention_user_ids.clear();
        assert!(
            worker
                .prepare_routable_inbound(follow_up, Some("self-id"))
                .is_none(),
            "routing ingress alone must not unlock unmentioned thread follow-ups"
        );
    }

    #[tokio::test]
    async fn mock_ws_identify_includes_direct_messages_and_routes_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(4);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            let identify = receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            assert_ne!(
                identify.d.unwrap()["intents"].as_u64().unwrap() & (1 << 12),
                0,
                "IDENTIFY must request DIRECT_MESSAGES"
            );
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(&mut socket, message_create("message-1", "human-1", false, None)).await;
            timeout(Duration::from_secs(1), async {
                tokio::select! {
                    _ = listener.accept() => panic!("runtime opened a duplicate Discord Gateway socket"),
                    _ = socket.next() => {}
                }
            })
            .await
            .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("routable message arrives")
            .expect("inbound sender remains live");
        assert!(!inbound.sender_is_bot());
        assert!(inbound.message().is_dm);
        assert_eq!(inbound.message().guild_id, None);
        assert_eq!(inbound.to_gateway_message().unwrap().thread_id, None);
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());

        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_filters_ready_self_authored_dispatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::All);
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(&mut socket, message_create("self-message", "self-id", true, Some("guild-1"))).await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        assert!(timeout(Duration::from_millis(100), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_preserves_verified_bot_provenance() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::All);
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            // Standard MESSAGE_CREATE carries neither parent_id nor
            // channel_type. THREAD_CREATE supplies the authoritative cache.
            send_payload(&mut socket, thread_create("channel-1", "parent-1")).await;
            send_payload(
                &mut socket,
                message_create("bot-message", "other-bot", true, Some("guild-1")),
            )
            .await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("bot policy admits the dispatch")
            .expect("inbound sender remains live");
        assert!(inbound.sender_is_bot());
        assert_eq!(inbound.message().guild_id.as_deref(), Some("guild-1"));
        assert_eq!(
            inbound.message().parent_channel_id.as_deref(),
            Some("parent-1")
        );
        assert_eq!(inbound.message().thread_id.as_deref(), Some("channel-1"));
        let routed = inbound.to_gateway_message().unwrap();
        assert_eq!(routed.user_id, "other-bot");
        assert_eq!(routed.chat_id, "parent-1");
        assert_eq!(routed.thread_id.as_deref(), Some("channel-1"));
        assert!(
            !runtime
                .thread_participation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains("channel-1"),
            "accepted ingress does not prove Hermes replied in the thread"
        );
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_resumes_then_resets_after_invalid_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(7, &url)).await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::RECONNECT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            let resume = receive_opcode(&mut second, opcodes::RESUME).await;
            assert_eq!(resume.d.unwrap()["session_id"], "session-1");
            send_payload(
                &mut second,
                GatewayPayload {
                    op: opcodes::INVALID_SESSION,
                    d: Some(serde_json::Value::Bool(false)),
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut third = accept(&listener).await;
            send_payload(&mut third, hello(60_000)).await;
            let identify = receive_opcode_with_timeout(
                &mut third,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            assert!(identify.d.is_some());
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            loop {
                if server.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mock gateway observes resume then identify");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_identify_spacing_starts_at_the_observed_first_write() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            // Connection preparation must not consume the IDENTIFY bucket.
            tokio::time::sleep(Duration::from_millis(100)).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            let first_identify_at = tokio::time::Instant::now();
            drop(first);

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            assert!(
                tokio::time::Instant::now().duration_since(first_identify_at)
                    >= Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS - 25),
                "the second server-observed IDENTIFY must honor the prior write timestamp"
            );
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, server)
            .await
            .expect("server observes paced second IDENTIFY")
            .expect("mock gateway task succeeds");
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_falls_back_to_primary_when_resume_endpoint_is_unreachable() {
        let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary_url = format!("ws://{}", primary.local_addr().unwrap());
        let unreachable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unreachable_url = format!("ws://{}", unreachable.local_addr().unwrap());
        drop(unreachable);
        let runtime = test_runtime(primary_url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&primary).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(7, &unreachable_url)).await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::RECONNECT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = timeout(Duration::from_secs(3), accept(&primary))
                .await
                .expect("primary Gateway receives fallback connection");
            send_payload(&mut second, hello(60_000)).await;
            let resume = receive_opcode(&mut second, opcodes::RESUME).await;
            assert_eq!(resume.d.unwrap()["session_id"], "session-1");
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            while !server.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resume falls back through the primary Gateway");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_saturated_inbound_queue_preserves_fifo_without_stalling_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(test_inbound_message("already-queued"))
            .expect("fixture fills bounded inbound queue");
        let _journal = attach_test_durable_inbound(&runtime, tx).await;
        let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, &url)).await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "retained-first",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "retained-second",
                    "human-1",
                    false,
                    Some("guild-1"),
                    4,
                ),
            )
            .await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            receive_opcode(&mut socket, opcodes::HEARTBEAT).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT_ACK,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            let _ = heartbeat_tx.send(());
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), heartbeat_rx)
            .await
            .expect("socket answers heartbeat while ingress is backpressured")
            .expect("mock gateway confirms heartbeat");
        let fixture = rx.recv().await.expect("fixture remains first");
        assert_eq!(fixture.message().message_id, "already-queued");
        let first = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("first retained dispatch reaches the drained queue")
            .expect("inbound receiver remains live");
        assert_eq!(first.message().message_id, "retained-first");
        let second = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("second retained dispatch follows the first")
            .expect("inbound receiver remains live");
        assert_eq!(second.message().message_id, "retained-second");
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_repeated_heartbeat_requests_cannot_extend_an_unacked_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(100)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, &url)).await;

            // Discord can request an immediate heartbeat repeatedly. Keep
            // asking without ACKing: a healthy client must reconnect at its
            // original pending-ACK/periodic deadline, not let OP 1 move it.
            for _ in 0..40 {
                let request = GatewayPayload {
                    op: opcodes::HEARTBEAT,
                    d: None,
                    s: None,
                    t: None,
                };
                let encoded = Message::Text(
                    serde_json::to_string(&request)
                        .expect("heartbeat request serializes")
                        .into(),
                );
                if first.send(encoded).await.is_err() {
                    break;
                }
                let response = timeout(Duration::from_millis(100), first.next()).await;
                let Ok(Some(Ok(Message::Text(text)))) = response else {
                    break;
                };
                let payload =
                    serde_json::from_str::<GatewayPayload>(text.as_str()).expect("heartbeat JSON");
                assert_eq!(payload.op, opcodes::HEARTBEAT);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let mut second = timeout(Duration::from_secs(1), accept(&listener))
                .await
                .expect("unacknowledged heartbeat reconnects despite repeated OP 1");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode(&mut second, opcodes::RESUME).await;
        });

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("heartbeat test server completes")
            .expect("heartbeat test server joins");
        runtime.stop().await.expect("runtime stops");
        journal.abort();
    }

    #[tokio::test]
    async fn mock_ws_retained_dispatch_survives_nonresumable_invalid_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(test_inbound_message("already-queued"))
            .expect("fixture fills bounded inbound queue");
        let _journal = attach_test_durable_inbound(&runtime, tx).await;
        let (identified_tx, identified_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(2, &url)).await;
            send_payload(
                &mut first,
                message_create_with_sequence(
                    "retained-after-reset",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::INVALID_SESSION,
                    d: Some(serde_json::Value::Bool(false)),
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            let _ = identified_tx.send(());
            timeout(Duration::from_secs(1), second.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, identified_rx)
            .await
            .expect("nonresumable reset reconnects with IDENTIFY")
            .expect("mock gateway observes IDENTIFY");
        let first = rx.recv().await.expect("fixture remains queued");
        assert_eq!(first.message().message_id, "already-queued");
        let retained = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("local pending dispatch survives invalid session reset")
            .expect("inbound receiver remains live");
        assert_eq!(retained.message().message_id, "retained-after-reset");
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_ready_resume_endpoint_falls_back_to_configured_gateway_with_identify() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, "http://unsupported-resume.example")).await;
            timeout(Duration::from_secs(1), first.next())
                .await
                .expect("invalid READY closes the first socket");

            let mut second = timeout(IDENTIFY_PACING_TEST_TIMEOUT, accept(&listener))
                .await
                .expect("configured gateway receives fallback connection");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            send_payload(&mut second, ready(2, &url)).await;
            send_payload(
                &mut second,
                message_create_with_sequence(
                    "after-malformed-ready",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
        });

        runtime.start().await.unwrap();
        let routed = timeout(IDENTIFY_PACING_TEST_TIMEOUT, inbound_rx.recv())
            .await
            .expect("fresh identify reconnect accepts later ingress")
            .expect("inbound receiver remains live");
        assert_eq!(routed.message().message_id, "after-malformed-ready");
        runtime.stop().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_closed_inbound_receiver_stops_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (tx, rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "closed-receiver",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("unhealthy worker closes its socket");
            assert!(
                timeout(Duration::from_millis(250), listener.accept())
                    .await
                    .is_err(),
                "closed inbound receiver must stop the worker rather than reconnect"
            );
        });

        runtime.start().await.unwrap();
        drop(rx);
        server.await.unwrap();
        runtime.stop().await;
    }

    #[tokio::test]
    async fn mock_ws_reconnects_at_next_interval_after_unacknowledged_heartbeat() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let resume_url = url.clone();
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(100)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, &resume_url)).await;
            receive_opcode(&mut first, opcodes::HEARTBEAT).await;

            let mut second = timeout(Duration::from_secs(2), accept(&listener))
                .await
                .expect("next fixed interval reconnects the unacknowledged socket");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode(&mut second, opcodes::RESUME).await;
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            while !server.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("unacknowledged heartbeat reconnects at the next interval");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_stop_cancels_live_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (identified_tx, identified_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            let _ = identified_tx.send(());
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("stop closes the live socket")
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), identified_rx)
            .await
            .expect("runtime identifies before shutdown")
            .expect("mock gateway observes IDENTIFY");
        timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("stop cancels the runtime");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_stop_during_heartbeat_loop_cancels_immediately() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel::<()>();
        let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(100)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            receive_opcode(&mut socket, opcodes::HEARTBEAT).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT_ACK,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            let _ = heartbeat_tx.send(());
            stopped_rx.await.expect("runtime stop completes");
            let terminal = timeout(Duration::from_millis(200), socket.next())
                .await
                .expect("stopped runtime closes socket");
            match terminal {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
                Some(Ok(frame)) => panic!("stopped runtime emitted another frame: {frame:?}"),
            }
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), heartbeat_rx)
            .await
            .expect("heartbeat loop starts")
            .expect("server observes heartbeat");
        timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("stop cancels the runtime");
        let _ = stopped_tx.send(());
        server.await.unwrap();
    }
}
