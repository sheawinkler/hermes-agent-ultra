    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::oneshot;

    struct BlockingLifecycleProvider {
        entered: std::sync::Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
    }

    impl BlockingLifecycleProvider {
        fn mark_entered(&self) {
            if let Some(sender) = self
                .entered
                .lock()
                .expect("blocking provider entry lock")
                .take()
            {
                let _ = sender.send(());
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for BlockingLifecycleProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.mark_entered();
            std::future::pending::<Result<hermes_core::LlmResponse, AgentError>>().await
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            self.mark_entered();
            futures::stream::pending().boxed()
        }
    }

    #[derive(Clone, Copy)]
    enum LifecycleRaceCause {
        Timeout,
        Cancellation,
    }

    struct OuterPollRaceProvider {
        interrupt: InterruptController,
        cause: LifecycleRaceCause,
        calls: AtomicUsize,
    }

    impl OuterPollRaceProvider {
        fn signal_after_outer_poll(&self) {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) != 0 {
                return;
            }
            match self.cause {
                LifecycleRaceCause::Timeout => self
                    .interrupt
                    .interrupt_timeout(Some("after outer select poll".to_string())),
                LifecycleRaceCause::Cancellation => self
                    .interrupt
                    .interrupt(Some("after outer select poll".to_string())),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for OuterPollRaceProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.signal_after_outer_poll();
            Ok(hermes_core::LlmResponse {
                message: Message::assistant(""),
                usage: None,
                model: "race".to_string(),
                finish_reason: None,
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            self.signal_after_outer_poll();
            futures::stream::iter(vec![Ok(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            })])
            .boxed()
        }
    }

    struct PanicLifecycleProvider;

    #[async_trait::async_trait]
    impl LlmProvider for PanicLifecycleProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            panic!("lifecycle provider panic")
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    struct RecordingMemoryProvider {
        calls: std::sync::Arc<AtomicUsize>,
    }

    impl crate::memory_manager::MemoryProviderPlugin for RecordingMemoryProvider {
        fn name(&self) -> &str {
            "effectful-test-memory"
        }

        fn prefetch(
            &self,
            _subject: &hermes_core::MemorySubject,
            _filter: &hermes_core::MemorySubjectStorageKey,
            _query: &str,
        ) -> String {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            String::new()
        }

        fn sync_turn(
            &self,
            _subject: &hermes_core::MemorySubject,
            _filter: &hermes_core::MemorySubjectStorageKey,
            _user: &str,
            _assistant: &str,
        ) {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        }

        fn on_turn_start(
            &self,
            _subject: &hermes_core::MemorySubject,
            _filter: &hermes_core::MemorySubjectStorageKey,
            _turn_number: u32,
            _message: &str,
        ) {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        }

        fn on_session_end(
            &self,
            _subject: &hermes_core::MemorySubject,
            _filter: &hermes_core::MemorySubjectStorageKey,
            _messages: &[serde_json::Value],
        ) {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

    struct NoCallProvider {
        calls: std::sync::Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for NoCallProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("unexpected"),
                usage: None,
                model: "no-call".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            futures::stream::empty().boxed()
        }
    }

    fn lifecycle_agent(
        session_id: &str,
        provider: std::sync::Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> (
        std::sync::Arc<AgentLoop>,
        std::sync::Arc<RunRuntime>,
        tempfile::TempDir,
    ) {
        let state = tempfile::tempdir().expect("lifecycle runtime directory");
        let runtime = std::sync::Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("lifecycle runtime"),
        );
        let agent = AgentLoop::with_interrupt(
            AgentConfig {
                session_id: Some(session_id.to_string()),
                max_turns: 4,
                ..AgentConfig::default()
            },
            std::sync::Arc::new(ToolRegistry::new()),
            provider,
            interrupt,
        )
        .with_run_runtime(runtime.clone());
        (std::sync::Arc::new(agent), runtime, state)
    }

    async fn wait_for_terminal_run(
        runtime: &RunRuntime,
        session_id: &str,
    ) -> crate::run_runtime::RunInspection {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let runs = runtime
                    .session_runs(
                        session_id,
                        crate::run_runtime::SessionRunSelection::All,
                    )
                    .expect("inspect lifecycle runs");
                if let Some(run) = runs.into_iter().next() {
                    if run.status.exit_terminal_reason.is_some() {
                        return run;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("run finalizer did not reach a terminal exit")
    }

    fn assert_exactly_one_terminal_exit(run: &crate::run_runtime::RunInspection) {
        assert!(run.status.exit_terminal_reason.is_some());
        assert_eq!(
            run.events
                .iter()
                .filter(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunExit
                })
                .count(),
            1,
            "run must have exactly one terminal exit: {:#?}",
            run.events
        );
    }

    #[tokio::test]
    async fn aborting_public_run_paths_recovers_exactly_one_terminal_exit() {
        for stream in [false, true] {
            let session_id = if stream {
                "abort-stream-lifecycle"
            } else {
                "abort-nonstream-lifecycle"
            };
            let (entered_tx, entered_rx) = oneshot::channel();
            let provider = std::sync::Arc::new(BlockingLifecycleProvider {
                entered: std::sync::Arc::new(std::sync::Mutex::new(Some(entered_tx))),
            });
            let (agent, runtime, _state) = lifecycle_agent(
                session_id,
                provider,
                InterruptController::new(),
            );
            let task = if stream {
                let agent = agent.clone();
                tokio::spawn(async move {
                    let (sink, _receiver) = AgentObserverSink::channel(8);
                    agent
                        .run_stream(vec![Message::user("abort stream")], None, sink)
                        .await
                })
            } else {
                let agent = agent.clone();
                tokio::spawn(async move { agent.run(vec![Message::user("abort")], None).await })
            };
            tokio::time::timeout(Duration::from_secs(1), entered_rx)
                .await
                .expect("provider was never entered")
                .expect("provider entry sender dropped");
            task.abort();
            assert!(task.await.expect_err("aborted task unexpectedly returned").is_cancelled());

            let run = wait_for_terminal_run(runtime.as_ref(), session_id).await;
            assert_exactly_one_terminal_exit(&run);
            tokio::time::sleep(Duration::from_millis(75)).await;
            let settled = runtime.inspect_run(run.run_id).expect("reinspect terminal run");
            assert_exactly_one_terminal_exit(&settled);
        }
    }

    #[tokio::test]
    async fn provider_panic_is_finalized_once() {
        let (agent, runtime, _state) = lifecycle_agent(
            "panic-lifecycle",
            std::sync::Arc::new(PanicLifecycleProvider),
            InterruptController::new(),
        );
        let result = agent.run(vec![Message::user("panic")], None).await;
        assert!(matches!(
            result,
            Err(AgentError::ToolExecution(message)) if message.contains("agent run panicked")
        ));
        let run = wait_for_terminal_run(runtime.as_ref(), "panic-lifecycle").await;
        assert_exactly_one_terminal_exit(&run);
    }

    #[tokio::test]
    async fn outer_select_race_preserves_timeout_and_cancellation_in_run_exits() {
        for (cause, session_id, expected_timeout) in [
            (LifecycleRaceCause::Timeout, "race-timeout-nonstream", true),
            (LifecycleRaceCause::Cancellation, "race-cancel-nonstream", false),
        ] {
            let interrupt = InterruptController::new();
            let provider = std::sync::Arc::new(OuterPollRaceProvider {
                interrupt: interrupt.clone(),
                cause,
                calls: AtomicUsize::new(0),
            });
            let (agent, runtime, _state) = lifecycle_agent(session_id, provider, interrupt);
            let result = agent.run(vec![Message::user("race")], None).await;
            if expected_timeout {
                assert!(matches!(
                    result,
                    Err(AgentError::Timeout(message)) if message == "after outer select poll"
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(AgentError::Interrupted { message })
                        if message.as_deref() == Some("after outer select poll")
                ));
            }
            let run = wait_for_terminal_run(runtime.as_ref(), session_id).await;
            assert_eq!(
                run.status.exit_terminal_reason,
                Some(if expected_timeout {
                    TerminalReason::Timeout
                } else {
                    TerminalReason::Cancellation
                })
            );
            assert_exactly_one_terminal_exit(&run);
        }
    }

    #[tokio::test]
    async fn outer_select_race_preserves_timeout_and_cancellation_for_stream_runs() {
        for (cause, session_id, expected_timeout) in [
            (LifecycleRaceCause::Timeout, "race-timeout-stream", true),
            (LifecycleRaceCause::Cancellation, "race-cancel-stream", false),
        ] {
            let interrupt = InterruptController::new();
            let provider = std::sync::Arc::new(OuterPollRaceProvider {
                interrupt: interrupt.clone(),
                cause,
                calls: AtomicUsize::new(0),
            });
            let (agent, runtime, _state) = lifecycle_agent(session_id, provider, interrupt);
            let (sink, _receiver) = AgentObserverSink::channel(8);
            let result = agent.run_stream(vec![Message::user("race")], None, sink).await;
            if expected_timeout {
                assert!(matches!(
                    result,
                    Err(AgentError::Timeout(message)) if message == "after outer select poll"
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(AgentError::Interrupted { message })
                        if message.as_deref() == Some("after outer select poll")
                ));
            }
            let run = wait_for_terminal_run(runtime.as_ref(), session_id).await;
            assert_eq!(
                run.status.exit_terminal_reason,
                Some(if expected_timeout {
                    TerminalReason::Timeout
                } else {
                    TerminalReason::Cancellation
                })
            );
            assert_exactly_one_terminal_exit(&run);
        }
    }

    #[tokio::test]
    async fn effect_capable_memory_is_blocked_before_any_memory_hook() {
        let memory_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let llm_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let state = tempfile::tempdir().expect("memory lifecycle runtime directory");
        let subject = hermes_core::MemorySubject::local_owner(
            hermes_core::MemorySurface::Local,
            "memory-preflight",
        )
        .expect("memory test subject");
        let mut manager =
            MemoryManager::new(subject, state.path()).expect("memory lifecycle manager");
        manager.add_provider(std::sync::Arc::new(RecordingMemoryProvider {
            calls: memory_calls.clone(),
        }));
        let runtime = std::sync::Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("memory lifecycle runtime"),
        );
        let agent = AgentLoop::new(
            AgentConfig {
                session_id: Some("memory-preflight".to_string()),
                ..AgentConfig::default()
            },
            std::sync::Arc::new(ToolRegistry::new()),
            std::sync::Arc::new(NoCallProvider {
                calls: llm_calls.clone(),
            }),
        )
        .with_memory(std::sync::Arc::new(std::sync::Mutex::new(manager)))
        .with_run_runtime(runtime.clone());

        let result = agent.run(vec![Message::user("memory")], None).await;
        assert!(matches!(
            result,
            Err(AgentError::PolicyBlocked(message))
                if message.contains("memory manager") && message.contains("typed run intents")
        ));
        assert_eq!(memory_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(llm_calls.load(AtomicOrdering::SeqCst), 0);
        let runs = runtime
            .session_runs(
                "memory-preflight",
                crate::run_runtime::SessionRunSelection::All,
            )
            .expect("blocked run is durably recorded");
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].status.exit_terminal_reason,
            Some(TerminalReason::PolicyBlocked)
        );
        assert_eq!(
            runs[0].header.provenance.prompt_digest(),
            &crate::run_ledger::ProvenanceDigest::absent(
                crate::run_ledger::DigestAbsence::Unavailable
            ),
            "blocked preflight records explicit unavailable prompt evidence"
        );
        assert_eq!(
            runs[0]
                .events
                .iter()
                .filter(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunStart
                })
                .count(),
            1,
            "blocked preflight still seals exactly one authoritative start"
        );
        assert_eq!(
            runs[0]
                .events
                .iter()
                .filter(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunExit
                })
                .count(),
            1,
            "blocked preflight produces one typed terminal exit"
        );
        assert!(
            runs[0]
                .events
                .iter()
                .find(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunStart
                })
                .and_then(|event| event.sealed_artifact_ref.as_ref())
                .is_some(),
            "blocked start retains sealed unavailable-prompt evidence"
        );
        assert!(
            runs[0]
                .events
                .iter()
                .all(|event| event.effect_intent == crate::run_ledger::EffectIntent::None),
            "blocked preflight records lifecycle evidence only, never a provider effect"
        );
    }

    #[tokio::test]
    async fn effect_capable_memory_stream_is_blocked_before_any_provider_or_effect() {
        let memory_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let llm_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let state = tempfile::tempdir().expect("stream memory lifecycle runtime directory");
        let subject = hermes_core::MemorySubject::local_owner(
            hermes_core::MemorySurface::Local,
            "memory-preflight-stream",
        )
        .expect("memory test subject");
        let mut manager =
            MemoryManager::new(subject, state.path()).expect("memory lifecycle manager");
        manager.add_provider(std::sync::Arc::new(RecordingMemoryProvider {
            calls: memory_calls.clone(),
        }));
        let runtime = std::sync::Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("memory lifecycle runtime"),
        );
        let agent = AgentLoop::new(
            AgentConfig {
                session_id: Some("memory-preflight-stream".to_string()),
                ..AgentConfig::default()
            },
            std::sync::Arc::new(ToolRegistry::new()),
            std::sync::Arc::new(NoCallProvider {
                calls: llm_calls.clone(),
            }),
        )
        .with_memory(std::sync::Arc::new(std::sync::Mutex::new(manager)))
        .with_run_runtime(runtime.clone());
        let (sink, _receiver) = AgentObserverSink::channel(8);
        let result = agent
            .run_stream(vec![Message::user("memory")], None, sink)
            .await;
        assert!(matches!(
            result,
            Err(AgentError::PolicyBlocked(message))
                if message.contains("memory manager") && message.contains("typed run intents")
        ));
        assert_eq!(memory_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(llm_calls.load(AtomicOrdering::SeqCst), 0);
        let runs = runtime
            .session_runs(
                "memory-preflight-stream",
                crate::run_runtime::SessionRunSelection::All,
            )
            .expect("blocked stream run is durably recorded");
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].status.exit_terminal_reason,
            Some(TerminalReason::PolicyBlocked)
        );
        assert_eq!(
            runs[0].header.provenance.prompt_digest(),
            &crate::run_ledger::ProvenanceDigest::absent(
                crate::run_ledger::DigestAbsence::Unavailable
            ),
            "blocked stream preflight records explicit unavailable prompt evidence"
        );
        assert_eq!(
            runs[0]
                .events
                .iter()
                .filter(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunStart
                })
                .count(),
            1
        );
        assert_eq!(
            runs[0]
                .events
                .iter()
                .filter(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunExit
                })
                .count(),
            1
        );
        assert!(
            runs[0]
                .events
                .iter()
                .find(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunStart)
                .and_then(|event| event.sealed_artifact_ref.as_ref())
                .is_some(),
            "blocked stream start retains sealed unavailable-prompt evidence"
        );
        assert!(
            runs[0]
                .events
                .iter()
                .all(|event| event.effect_intent == crate::run_ledger::EffectIntent::None),
            "blocked stream preflight records lifecycle evidence only, never a provider effect"
        );
    }
