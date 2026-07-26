    struct PreparedInputProvider;

    #[async_trait::async_trait]
    impl LlmProvider for PreparedInputProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("unused"),
                usage: None,
                model: "prepared-input-test".to_string(),
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
            use futures::StreamExt;
            futures::stream::empty().boxed()
        }
    }

    #[derive(Default)]
    struct PreparedExecutionRecordingProvider {
        nonstream: std::sync::Mutex<Vec<(Vec<Message>, Vec<ToolSchema>)>>,
        stream: std::sync::Mutex<Vec<(Vec<Message>, Vec<ToolSchema>)>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for PreparedExecutionRecordingProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.nonstream
                .lock()
                .expect("record nonstream request")
                .push((messages.to_vec(), tools.to_vec()));
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "prepared-execution-test".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            messages: &[Message],
            tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            use futures::StreamExt;

            self.stream
                .lock()
                .expect("record stream request")
                .push((messages.to_vec(), tools.to_vec()));
            futures::stream::iter(vec![
                Ok(StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: Some("done".to_string()),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                }),
                Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some("stop".to_string()),
                    usage: None,
                }),
            ])
            .boxed()
        }
    }

    struct AllowPreparedStartEvidenceRead;

    impl crate::sealed_artifact::ExactArtifactAuthorization for AllowPreparedStartEvidenceRead {
        fn authorize_exact_read(
            &self,
            _artifact: &crate::sealed_artifact::SealedArtifactRef,
        ) -> Result<(), crate::sealed_artifact::SealedArtifactError> {
            Ok(())
        }
    }

    async fn install_attested_prepared_prompt_skill(root: &std::path::Path) {
        use hermes_core::Skill;
        use hermes_skills::{
            store_local_owner_keyring_file, FileSkillStore, LocalOwnerKeyring,
            LocalOwnerSigningKey, SkillStore,
        };

        std::fs::create_dir_all(root).expect("create skill root");
        let home = root.parent().expect("skills root has a home");
        let layout = hermes_config::SkillTrustAuthorityLayout::from_resolved_home(home)
            .expect("derive skill trust authority layout");
        assert_eq!(layout.skills_root(), root);
        std::fs::create_dir_all(layout.authority_root()).expect("create skill authority root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                layout.authority_root(),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("restrict skill authority root");
        }

        let signer = LocalOwnerSigningKey::from_seed([41; 32]);
        let local_owner_keys = vec![signer.trusted_key(
            "prepared-prompt-owner",
            "prepared-prompt-key",
            true,
        )];
        let keyring =
            LocalOwnerKeyring::new(local_owner_keys.clone()).expect("create local owner keyring");
        store_local_owner_keyring_file(root, layout.local_owner_keyring_file(), &keyring)
            .expect("store local owner keyring");

        let store =
            FileSkillStore::new(root.to_path_buf()).with_trusted_local_owner_keys(local_owner_keys);
        store
            .save(&Skill {
                name: "demo-skill".to_string(),
                content: "Body".to_string(),
                category: Some("testing".to_string()),
                description: Some("Prepared provenance fixture".to_string()),
            })
            .await
            .expect("stage local skill");
        let mut manifest = store
            .pending_local_manifest("demo-skill")
            .expect("load staged local skill")
            .expect("staged local skill manifest");
        let attestation = signer
            .sign_local_manifest(
                &mut manifest,
                "prepared-prompt-owner",
                "prepared-prompt-key",
                "prepared-prompt-attestation",
                "2026-07-20T00:00:00Z",
            )
            .expect("sign local skill attestation");
        store
            .attest_local_skill("demo-skill", attestation)
            .expect("attest local skill");
    }

    #[tokio::test]
    async fn prepared_input_applies_shared_sanitization_and_context_expansion() {
        let config = AgentConfig {
            skip_context_files: true,
            skip_memory: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(PreparedInputProvider),
        );
        let user_content = "inspect @file:Cargo.toml";
        let cwd = std::env::current_dir().expect("test working directory");
        let expected = preprocess_context_references_async(
            user_content,
            &cwd,
            get_model_context_length(&config.model),
            None,
        )
        .await;
        assert!(expected.expanded, "workspace Cargo.toml should expand");
        let advertised = vec![ToolSchema::new(
            "observation",
            "prepared input test tool",
            hermes_core::JsonSchema::new("object"),
        )];
        let prepared = agent
            .prepare_run_input(
                vec![
                    Message::tool_result("old-tool", r#"{"ok":true,"_budget_warning":"transient"}"#),
                    Message::user(user_content),
                ],
                Some(advertised.clone()),
            )
            .await
            .expect("prepared input");

        assert_eq!(prepared.advertised_tools, advertised);
        assert_eq!(
            prepared.initial_messages[1].content.as_deref(),
            Some(expected.message.as_str()),
            "prepared input must use the bounded context-reference result"
        );
        let stripped: serde_json::Value = serde_json::from_str(
            prepared.initial_messages[0]
                .content
                .as_deref()
                .expect("tool content"),
        )
        .expect("sanitized tool JSON");
        assert!(
            stripped
                .as_object()
                .expect("tool JSON object")
                .get("_budget_warning")
                .is_none(),
            "transient budget warnings must not enter start evidence or execution"
        );
        assert!(matches!(
            prepared.system_prompt,
            ResolvedSystemPrompt::Fresh(FreshSystemPrompt {
                injected_skills_block: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stored_prompt_skips_fresh_skill_scan_while_fresh_prompt_retains_exact_block() {
        let home = tempfile::tempdir().expect("skills home");
        let skills_root = home.path().join("skills");
        install_attested_prepared_prompt_skill(&skills_root).await;
        let tools = vec![ToolSchema::new(
            "skills_list",
            "List skills",
            hermes_core::JsonSchema::new("object"),
        )];
        let fresh_config = AgentConfig {
            hermes_home: Some(home.path().to_string_lossy().to_string()),
            skip_context_files: true,
            skip_memory: true,
            ..AgentConfig::default()
        };
        let fresh = AgentLoop::new(
            fresh_config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(PreparedInputProvider),
        )
        .prepare_run_input(vec![Message::user("fresh")], Some(tools.clone()))
        .await
        .expect("fresh prepared input");
        match fresh.system_prompt {
            ResolvedSystemPrompt::Fresh(prompt) => {
                let block = prompt
                    .injected_skills_block
                    .as_deref()
                    .expect("fresh prompt must retain injected skills");
                assert!(block.contains("demo-skill"));
                assert!(
                    block.contains("<skill_provenance"),
                    "fresh injected block retains exact per-skill provenance"
                );
                assert!(prompt.content.contains(block));
            }
            _ => panic!("fresh configuration must build a fresh prompt"),
        }

        let stored = AgentLoop::new(
            AgentConfig {
                stored_system_prompt: Some("stored exact system prompt".to_string()),
                ..fresh_config
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(PreparedInputProvider),
        )
        .prepare_run_input(vec![Message::user("stored")], Some(tools))
        .await
        .expect("stored prepared input");
        match stored.system_prompt {
            ResolvedSystemPrompt::Stored { content } => {
                assert_eq!(content, "stored exact system prompt");
            }
            _ => panic!("stored prompt must not perform fresh skill discovery"),
        }
    }

    #[tokio::test]
    async fn prepared_input_redacts_credentials_before_task_hint_and_execution() {
        let secret = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
        let agent = AgentLoop::new(
            AgentConfig {
                stored_system_prompt: Some(format!("stored token: {secret}")),
                skip_context_files: true,
                skip_memory: true,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(PreparedInputProvider),
        );
        let prepared = agent
            .prepare_run_input(
                vec![
                    Message::tool_result(
                        "old",
                        format!(r#"{{"api_key":"{secret}","ok":true}}"#),
                    ),
                    Message::user(format!("use credential {secret}")),
                ],
                None,
            )
            .await
            .expect("credential-safe prepared input");

        let user = prepared.initial_messages[1]
            .content
            .as_deref()
            .expect("redacted user content");
        assert!(!user.contains(secret));
        assert_ne!(user, format!("use credential {secret}"));
        let tool = prepared.initial_messages[0]
            .content
            .as_deref()
            .expect("redacted structured tool content");
        assert!(!tool.contains(secret));
        assert!(tool.contains("[REDACTED_SECRET]"));
        match prepared.system_prompt {
            ResolvedSystemPrompt::Stored { content } => {
                assert!(!content.contains(secret));
                assert_ne!(content, format!("stored token: {secret}"));
            }
            _ => panic!("stored source remains stored after sanitization"),
        }
    }

    #[tokio::test]
    async fn tool_schema_property_names_are_preserved_and_secret_literals_are_rejected() {
        let agent = AgentLoop::new(
            AgentConfig {
                skip_context_files: true,
                skip_memory: true,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(PreparedInputProvider),
        );
        let mut safe_parameters = hermes_core::JsonSchema::object(Default::default(), vec![
            "api_key".to_string(),
        ]);
        safe_parameters
            .properties
            .as_mut()
            .expect("object properties")
            .insert("api_key".to_string(), serde_json::json!({"type":"string"}));
        let safe_schema = ToolSchema::new("example", "safe schema", safe_parameters);
        let prepared = agent
            .prepare_run_input(
                vec![Message::user("schema")],
                Some(vec![safe_schema.clone()]),
            )
            .await
            .expect("api_key property is structural, not a secret value");
        assert_eq!(prepared.advertised_tools, vec![safe_schema]);

        let secret = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
        let mut unsafe_parameters = hermes_core::JsonSchema::object(Default::default(), vec![
            "api_key".to_string(),
        ]);
        unsafe_parameters
            .properties
            .as_mut()
            .expect("object properties")
            .insert(
                "api_key".to_string(),
                serde_json::json!({"type":"string","default":secret}),
            );
        match agent
            .prepare_run_input(
                vec![Message::user("unsafe schema")],
                Some(vec![ToolSchema::new(
                    "example",
                    "unsafe schema",
                    unsafe_parameters,
                )]),
            )
            .await
        {
            Err(AgentError::Config(message)) if message.contains("credential-like literal") => {}
            Err(other) => panic!("unexpected schema rejection: {other}"),
            Ok(_) => panic!("secret default must reject before run admission"),
        }
    }

    #[tokio::test]
    async fn sealed_prepared_start_evidence_matches_both_execution_paths() {
        let state = tempfile::tempdir().expect("prepared execution runtime state");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("prepared execution runtime"),
        );
        let secret = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
        let nonstream_user = format!("nonstream private value {secret}");
        let stream_user = format!("stream private value {secret}");
        let mut parameters = hermes_core::JsonSchema::object(Default::default(), vec![
            "api_key".to_string(),
        ]);
        parameters
            .properties
            .as_mut()
            .expect("tool schema properties")
            .insert("api_key".to_string(), serde_json::json!({"type":"string"}));
        let advertised = vec![ToolSchema::new(
            "prepared-tool",
            "prepared execution tool",
            parameters,
        )];
        let config = AgentConfig {
            session_id: Some("prepared-execution-parity".to_string()),
            stored_system_prompt: Some(format!("stored private system {secret}")),
            skip_context_files: true,
            skip_memory: true,
            ..AgentConfig::default()
        };
        let provider = Arc::new(PreparedExecutionRecordingProvider::default());
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            provider.clone(),
        )
        .with_run_runtime(runtime.clone());

        agent
            .run(
                vec![Message::user(nonstream_user)],
                Some(advertised.clone()),
            )
            .await
            .expect("nonstream prepared run");
        let (sink, _receiver) = AgentObserverSink::channel(8);
        agent
            .run_stream(vec![Message::user(stream_user)], Some(advertised.clone()), sink)
            .await
            .expect("stream prepared run");

        // Quality/finalizer retries may add later model calls; assert the first
        // prepared request rather than coupling evidence parity to that count.
        let nonstream_requests = provider
            .nonstream
            .lock()
            .expect("read nonstream requests")
            .clone();
        let stream_requests = provider
            .stream
            .lock()
            .expect("read stream requests")
            .clone();
        assert!(
            !nonstream_requests.is_empty(),
            "nonstream execution must reach the provider"
        );
        assert!(
            !stream_requests.is_empty(),
            "stream execution must reach the provider"
        );
        for (execution_messages, execution_tools) in
            nonstream_requests.iter().chain(stream_requests.iter())
        {
            assert_eq!(
                execution_tools, &advertised,
                "execution advertises the sealed schemas unchanged"
            );
            assert!(
                !execution_messages
                    .iter()
                    .filter_map(|message| message.content.as_deref())
                    .any(|content| content.contains(secret)),
                "sanitized execution never receives the private sentinel"
            );
        }
        let runs = runtime
            .session_runs(
                "prepared-execution-parity",
                crate::run_runtime::SessionRunSelection::All,
            )
            .expect("prepared execution runs");
        assert_eq!(runs.len(), 2);
        for run in runs {
            let start = run
                .events
                .iter()
                .find(|event| {
                    event.kind == crate::run_ledger::SessionRunEventKind::RunStart
                })
                .expect("sealed run start");
            let exact = runtime
                .exact_replay_lifecycle()
                .read_exact(
                    run.run_id,
                    start.event_id,
                    start
                        .sealed_artifact_ref
                        .as_ref()
                        .expect("sealed start reference"),
                    &AllowPreparedStartEvidenceRead,
                )
                .expect("authorized prepared start read");
            let exact: serde_json::Value =
                serde_json::from_slice(exact.as_slice()).expect("prepared exact start object");
            assert_eq!(exact["system_prompt_source"], "stored");
            assert!(exact["injected_skills_block"].is_null());
            assert_eq!(
                exact["advertised_tools"],
                serde_json::to_value(&advertised).expect("serialize advertised tools")
            );
            assert_eq!(
                run.header.provenance.prompt_digest(),
                &crate::run_ledger::ProvenanceDigest::absent(
                    crate::run_ledger::DigestAbsence::Unavailable
                )
            );
            assert_eq!(
                run.header.provenance.input_digest(),
                &crate::run_ledger::ProvenanceDigest::absent(
                    crate::run_ledger::DigestAbsence::Unavailable
                )
            );
            let exact_user = exact["messages"]
                .as_array()
                .and_then(|messages| {
                    messages
                        .iter()
                        .find(|message| message["role"] == "user")
                })
                .and_then(|message| message["content"].as_str())
                .expect("sealed initial user content");
            let path_requests = if exact_user.contains("nonstream") {
                &nonstream_requests
            } else if exact_user.contains("stream") {
                &stream_requests
            } else {
                panic!("unrecognized sealed initial input: {exact_user}");
            };
            assert!(
                path_requests
                    .iter()
                    .any(|(execution_messages, execution_tools)| {
                        execution_tools == &advertised
                            && execution_messages.len() == 2
                            && execution_messages[0].content.as_deref()
                                == exact["system_prompt"].as_str()
                            && serde_json::to_value(&execution_messages[1..])
                                .expect("serialize execution initial messages")
                                == exact["messages"]
                    }),
                "the execution path must receive exactly the sealed prepared input"
            );
            let export = runtime
                .publish_run_export(run.run_id)
                .expect("prepared public run export");
            let export_text = std::fs::read_to_string(export.path).expect("read public export");
            assert!(!export_text.contains(secret));
        }
    }
