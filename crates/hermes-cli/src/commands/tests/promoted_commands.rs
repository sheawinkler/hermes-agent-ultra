    #[tokio::test]
    async fn cli_reload_skills_reports_snapshot_and_queues_note() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let skill_dir = tmp.path().join("skills").join("release-captain");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Release Captain\ndescription: Release workflow\n---\n# Release Captain\n1. Inspect changed files\n",
        )
        .expect("write skill");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/reload-skills", &[])
            .await
            .expect("reload skills");

        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Reloaded installed skill commands"));
        assert!(out.contains("/release-captain"));
        assert!(out.contains("no prompt cache was invalidated"));
        assert_eq!(app.pending_system_note_count(), 1);
    }

    #[tokio::test]
    async fn promoted_snapshot_command_lists_snapshots() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_snapshot_command(&mut app, &[]).expect("snapshot list");
        assert_eq!(result, CommandResult::Handled);

        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Session snapshots:") || output.contains("No snapshots found in"));
    }

    #[tokio::test]
    async fn promoted_rollback_command_shows_controls() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_rollback_command(&mut app, &[]).expect("rollback list");
        assert_eq!(result, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Rollback controls:"));
    }

    #[tokio::test]
    async fn promoted_queue_command_shows_usage_and_status() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let usage = handle_queue_command(&mut app, &[]).expect("queue usage");
        assert_eq!(usage, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Usage: /queue <prompt>"));

        let status = handle_queue_command(&mut app, &["status"]).expect("queue status");
        assert_eq!(status, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Background queue status:"));
    }

    #[tokio::test]
    async fn promoted_steer_command_sets_and_clears_instruction() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_steer_command(&mut app, &["focus", "on", "repo", "map"]).expect("set steer");
        assert_eq!(
            current_session_steer(&app).as_deref(),
            Some("focus on repo map")
        );
        assert!(latest_ui_assistant_text(&app).contains("Steering instruction set."));

        handle_steer_command(&mut app, &["clear"]).expect("clear steer");
        assert!(current_session_steer(&app).is_none());
        assert!(latest_ui_assistant_text(&app).contains("Cleared session steering instruction."));
    }

    #[test]
    fn acp_steer_prompt_interrupts_with_trusted_marker() {
        let session_id = "session-steer-marker".to_string();
        let controller = hermes_agent::InterruptController::new();
        let interrupts = Arc::new(Mutex::new(HashMap::from([(
            session_id.clone(),
            controller.clone(),
        )])));
        let executor = CliAcpPromptExecutor {
            config: Arc::new(GatewayConfig::default()),
            tool_registry: Arc::new(hermes_tools::ToolRegistry::new()),
            interrupts,
        };
        let session = hermes_acp::SessionState::new(session_id, ".".to_string());

        assert!(hermes_acp::AcpPromptExecutor::steer_prompt(
            &executor,
            &session,
            "prefer the simpler fix"
        )
        .expect("steer prompt"));

        let marker = controller
            .take_interrupt_graceful()
            .expect("interrupt set")
            .expect("marker");
        assert!(marker.contains(hermes_agent::STEER_MARKER_OPEN));
        assert!(marker.contains("prefer the simpler fix"));
        assert!(marker.contains(hermes_agent::STEER_MARKER_CLOSE));
        assert!(!marker.contains("User guidance:"));
    }

    struct CliNoopTool {
        schema: hermes_core::ToolSchema,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for CliNoopTool {
        async fn execute(
            &self,
            _params: serde_json::Value,
        ) -> Result<String, hermes_core::ToolError> {
            Ok("ok".to_string())
        }

        fn schema(&self) -> hermes_core::ToolSchema {
            self.schema.clone()
        }
    }

    fn register_cli_noop_tool(registry: &hermes_tools::ToolRegistry, name: &str) {
        let schema =
            hermes_core::tool_schema(name, "CLI noop", hermes_core::JsonSchema::new("object"));
        registry.register(
            name,
            "mcp-test",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "CLI noop",
            "mcp",
            None,
        );
    }

    #[tokio::test]
    async fn reload_mcp_refreshes_agent_snapshot_from_runtime_registry() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        assert!(app.agent.tool_registry.get("mcp_srv_ping").is_none());
        register_cli_noop_tool(&app.tool_registry, "mcp_srv_ping");

        let result = handle_reload_command(&mut app, "/reload-mcp").expect("reload mcp");

        assert_eq!(result, CommandResult::Handled);
        assert!(app.agent.tool_registry.get("mcp_srv_ping").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("MCP reload complete"));
        assert!(out.contains("Added: mcp_srv_ping"));
    }

    #[test]
    fn acp_prompt_executor_respects_mcp_toolset_gate() {
        let mut config = GatewayConfig::default();
        config
            .platform_toolsets
            .insert("cli".to_string(), vec!["file".to_string()]);
        let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
        let executor = CliAcpPromptExecutor {
            config: Arc::new(config),
            tool_registry: Arc::clone(&tool_registry),
            interrupts: Arc::new(Mutex::new(HashMap::new())),
        };
        let file_schema = hermes_core::tool_schema(
            "read_file",
            "Read file",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "read_file",
            "file",
            file_schema.clone(),
            Arc::new(CliNoopTool {
                schema: file_schema,
            }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "Read file",
            "file",
            None,
        );
        assert!(executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "read_file"));
        assert!(!executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));

        let schema = hermes_core::tool_schema(
            "mcp_srv_ping",
            "MCP ping",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "mcp_srv_ping",
            "mcp-srv",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "MCP ping",
            "mcp",
            None,
        );

        assert!(!executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
    }

    #[test]
    fn acp_prompt_executor_allows_explicit_mcp_toolset_alias() {
        let mut config = GatewayConfig::default();
        config
            .platform_toolsets
            .insert("cli".to_string(), vec!["srv".to_string()]);
        let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
        let executor = CliAcpPromptExecutor {
            config: Arc::new(config),
            tool_registry: Arc::clone(&tool_registry),
            interrupts: Arc::new(Mutex::new(HashMap::new())),
        };
        let schema = hermes_core::tool_schema(
            "mcp_srv_ping",
            "MCP ping",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "mcp_srv_ping",
            "mcp-srv",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "MCP ping",
            "mcp",
            None,
        );
        tool_registry.register_toolset_alias("srv", "mcp-srv");

        assert!(executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
    }

    #[tokio::test]
    async fn promoted_btw_command_queues_ephemeral_background_task() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result =
            handle_btw_command(&mut app, &["why", "is", "latency", "high?"]).expect("btw command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("[/btw queued]"));
        assert!(output.contains("Question: why is latency high?"));
    }

    #[tokio::test]
    async fn slash_auth_status_command_is_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/auth", &["status"])
            .await
            .expect("auth status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Auth status"));
    }

    #[tokio::test]
    async fn slash_runbook_and_telemetry_commands_are_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let runbook = handle_slash_command(&mut app, "/runbook", &["list"])
            .await
            .expect("runbook list");
        assert_eq!(runbook, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Runbooks"));

        let telemetry = handle_slash_command(&mut app, "/telemetry", &["status"])
            .await
            .expect("telemetry status");
        assert_eq!(telemetry, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Telemetry snapshot"));
    }

    #[tokio::test]
    async fn slash_agents_pause_resume_and_status_are_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        std::env::remove_var("HERMES_DELEGATION_PAUSED");

        let status = handle_slash_command(&mut app, "/agents", &["status"])
            .await
            .expect("agents status");
        assert_eq!(status, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Delegation spawning: active"));

        let pause = handle_slash_command(&mut app, "/agents", &["pause"])
            .await
            .expect("agents pause");
        assert_eq!(pause, CommandResult::Handled);
        assert_eq!(
            std::env::var("HERMES_DELEGATION_PAUSED").ok().as_deref(),
            Some("1")
        );
        assert!(latest_ui_assistant_text(&app).contains("paused for this runtime"));

        let resume = handle_slash_command(&mut app, "/agents", &["resume"])
            .await
            .expect("agents resume");
        assert_eq!(resume, CommandResult::Handled);
        assert_eq!(
            std::env::var("HERMES_DELEGATION_PAUSED").ok().as_deref(),
            Some("0")
        );
        assert!(latest_ui_assistant_text(&app).contains("resumed for this runtime"));
    }

    #[tokio::test]
    async fn slash_agents_doctor_uses_native_queue_audit() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let jobs = tmp.path().join("background_jobs");
        std::fs::create_dir_all(&jobs).expect("jobs dir");
        std::fs::write(
            jobs.join("one.json"),
            r#"{"id":"dupe","status":"running","task":"inspect"}"#,
        )
        .expect("write one");
        std::fs::write(
            jobs.join("two.json"),
            r#"{"id":"dupe","status":"queued","task":"inspect again"}"#,
        )
        .expect("write two");
        std::fs::write(jobs.join("bad.json"), "{not json").expect("write bad");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/agents", &["doctor"])
            .await
            .expect("agents doctor");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Queue manifest audit (native)"));
        assert!(out.contains("json=3"));
        assert!(out.contains("malformed=1"));
        assert!(out.contains("duplicate_ids=1"));
        assert!(!out.contains("audit_background_queue.py"));
    }

    #[tokio::test]
    async fn promoted_sethome_command_sets_status_and_clears_marker() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_sethome_command(&mut app, &["alpha-room"]).expect("set home");
        assert!(latest_ui_assistant_text(&app).contains("Home marker updated."));
        let marker = load_home_session_marker().expect("home marker");
        assert_eq!(
            marker.get("home").and_then(|v| v.as_str()),
            Some("alpha-room")
        );

        handle_sethome_command(&mut app, &["status"]).expect("home status");
        assert!(latest_ui_assistant_text(&app).contains("Home marker file:"));

        handle_sethome_command(&mut app, &["clear"]).expect("home clear");
        assert!(latest_ui_assistant_text(&app).contains("Cleared home marker."));
        assert!(load_home_session_marker().is_none());
    }

    #[tokio::test]
    async fn promoted_paste_command_uses_test_clipboard_override() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        std::env::set_var("HERMES_TEST_CLIPBOARD_TEXT", "alpha clipboard payload");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_paste_command(&mut app, &[]).expect("paste command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Clipboard captured:"));
        assert!(output.contains("alpha clipboard payload"));
    }

    #[tokio::test]
    async fn promoted_gquota_command_emits_provider_diagnostics() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_gquota_command(&mut app, &[]).await.expect("gquota");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Gemini quota/auth diagnostics"));
        assert!(output.contains("active provider:"));
    }

    #[tokio::test]
    async fn promoted_image_command_queues_and_consumes_hint() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result =
            handle_image_command(&mut app, &["/tmp/example-image.png"]).expect("image queue");
        assert_eq!(result, CommandResult::Handled);
        assert_eq!(app.pending_image_hint(), Some("/tmp/example-image.png"));
        assert!(latest_ui_assistant_text(&app).contains("Image hint queued"));

        let prepared = app.prepare_user_message("analyze the screenshot");
        assert!(prepared.starts_with("[IMAGE_HINT] path=/tmp/example-image.png"));
        assert!(app.pending_image_hint().is_none());

        let cleared = handle_image_command(&mut app, &["clear"]).expect("image clear");
        assert_eq!(cleared, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Cleared pending image hint"));
    }

    #[tokio::test]
    async fn promoted_feedback_command_writes_feedback_log() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_feedback_command(&mut app, &["solid", "repro", "steps"])
            .expect("feedback write");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Feedback captured in"));

        let path = feedback_log_path();
        let raw = std::fs::read_to_string(&path).expect("read feedback log");
        assert!(raw.contains("\"note\":\"solid repro steps\""));
    }

    #[tokio::test]
    async fn promoted_debug_dump_command_writes_session_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        app.messages.push(hermes_core::Message::user("hello"));
        let result = handle_debug_dump_command(&mut app, &[]).expect("debug dump");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Debug snapshot written."));

        let sessions_dir = app.state_root.join("sessions");
        let count = std::fs::read_dir(sessions_dir)
            .expect("sessions dir")
            .filter_map(|entry| entry.ok())
            .count();
        assert!(count > 0);
    }

    #[tokio::test]
    async fn promoted_plan_status_command_emits_queue_summary() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_plan_command(&mut app, &["status"]).expect("plan status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Planner queue status"));
        assert!(output.contains("queued="));
    }

    #[tokio::test]
    async fn promoted_lsp_status_command_emits_index_details() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_lsp_command(&mut app, &["status"]).expect("lsp status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("LSP/code-index status"));
        assert!(output.contains("code_index_enabled"));
    }

    #[tokio::test]
    async fn promoted_approve_and_deny_commands_operate_on_pairing_store() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let store = PairingStore::open_default();
        store
            .save(&[crate::pairing_store::PairedDevice {
                device_id: "device-01".to_string(),
                name: Some("Test device".to_string()),
                status: PairingStatus::Pending,
                created_at: chrono::Utc::now().to_rfc3339(),
                last_seen: None,
                shared_secret: None,
            }])
            .expect("seed pairing store");

        handle_approve_command(&mut app, &["device-01"]).expect("approve");
        assert!(latest_ui_assistant_text(&app).contains("Approved device 'device-01'"));

        handle_deny_command(&mut app, &["device-01"]).expect("deny");
        assert!(latest_ui_assistant_text(&app).contains("Revoked device 'device-01'"));
    }

    #[test]
    fn test_acp_history_to_messages_preserves_multimodal_user_content_marker() {
        let history = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "check this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
            ]
        })];
        let messages = acp_history_to_messages(&history, "");
        assert_eq!(messages.len(), 1);
        let content = messages[0].content.as_deref().unwrap_or("");
        assert!(content.starts_with(ACP_MULTIMODAL_PREFIX));
    }

    #[test]
    fn test_acp_history_to_messages_flattens_assistant_parts_to_text() {
        let history = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "done"},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
            ]
        })];
        let messages = acp_history_to_messages(&history, "");
        assert_eq!(messages.len(), 1);
        let content = messages[0].content.as_deref().unwrap_or("");
        assert!(content.contains("done"));
        assert!(content.contains("Attached image"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_pairs_tool_results_by_call_id() {
        let messages = vec![
            hermes_core::Message::assistant_with_tool_calls(
                Some("checking files".to_string()),
                vec![
                    hermes_core::ToolCall {
                        id: "tc-read".to_string(),
                        function: hermes_core::FunctionCall {
                            name: "read_file".to_string(),
                            arguments: r#"{"path":"/etc/hosts"}"#.to_string(),
                        },
                        extra_content: None,
                    },
                    hermes_core::ToolCall {
                        id: "tc-web".to_string(),
                        function: hermes_core::FunctionCall {
                            name: "web_search".to_string(),
                            arguments: r#"{"query":"rust acp"}"#.to_string(),
                        },
                        extra_content: None,
                    },
                ],
            ),
            hermes_core::Message::tool_result("tc-read", "127.0.0.1 localhost"),
            hermes_core::Message::tool_result("tc-web", r#"{"data":{"web":[]}}"#),
        ];

        let events = acp_events_from_agent_messages("session-1", &messages);

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(events[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(events[0].arguments.as_ref().unwrap()["path"], "/etc/hosts");
        assert_eq!(events[1].tool_call_id.as_deref(), Some("tc-web"));
        assert_eq!(events[2].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(events[2].tool_name.as_deref(), Some("read_file"));
        assert_eq!(events[2].result.as_deref(), Some("127.0.0.1 localhost"));
        assert_eq!(events[3].tool_call_id.as_deref(), Some("tc-web"));
        assert_eq!(events[3].tool_name.as_deref(), Some("web_search"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_emits_native_todo_plan() {
        let todo_result = r#"{"todos":[{"id":"inspect","content":"Inspect ACP","status":"completed"},{"id":"patch","content":"Patch renderer","status":"in_progress"}]}"#;
        let messages = vec![
            hermes_core::Message::assistant_with_tool_calls(
                None,
                vec![hermes_core::ToolCall {
                    id: "tc-todo".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "todo".to_string(),
                        arguments: r#"{"todos":[]}"#.to_string(),
                    },
                    extra_content: None,
                }],
            ),
            hermes_core::Message::tool_result("tc-todo", todo_result),
        ];

        let events = acp_events_from_agent_messages("session-1", &messages);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, hermes_acp::AcpEventKind::ToolCallStart);
        assert_eq!(events[1].kind, hermes_acp::AcpEventKind::ToolCallComplete);
        assert_eq!(events[2].kind, hermes_acp::AcpEventKind::PlanUpdate);
        assert_eq!(events[2].session_update.as_deref(), Some("plan"));
        let entries = events[2].entries.as_ref().expect("plan entries");
        assert_eq!(entries[0].content, "Inspect ACP");
        assert_eq!(entries[0].status, "completed");
        assert_eq!(entries[1].content, "Patch renderer");
        assert_eq!(entries[1].status, "in_progress");
    }

    #[test]
    fn test_acp_stream_callbacks_route_reasoning_and_message_deltas() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let callbacks = acp_stream_callbacks("session-1", events.clone());

        callbacks.on_thinking.as_ref().unwrap()("actual reasoning");
        callbacks.on_stream_delta.as_ref().unwrap()("streamed answer");
        callbacks.on_thinking.as_ref().unwrap()("   ");
        callbacks.on_stream_delta.as_ref().unwrap()("");

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, hermes_acp::AcpEventKind::AgentThoughtChunk);
        assert_eq!(
            events[0].session_update.as_deref(),
            Some("agent_thought_chunk")
        );
        assert_eq!(events[0].text.as_deref(), Some("actual reasoning"));
        assert_eq!(events[1].kind, hermes_acp::AcpEventKind::MessageDelta);
        assert_eq!(events[1].text.as_deref(), Some("streamed answer"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_uses_fallback_for_untracked_tool_result() {
        let mut result = hermes_core::Message::tool_result("tc-untracked", "ok");
        result.name = Some("terminal".to_string());

        let events = acp_events_from_agent_messages("session-1", &[result]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_call_id.as_deref(), Some("tc-untracked"));
        assert_eq!(events[0].tool_name.as_deref(), Some("terminal"));
        assert_eq!(events[0].result.as_deref(), Some("ok"));
    }

    #[test]
    fn test_acp_usage_from_agent_usage_maps_top_level_agent_fields() {
        let usage = hermes_core::UsageStats {
            prompt_tokens: 123,
            completion_tokens: 45,
            total_tokens: 168,
            estimated_cost: Some(0.0123),
        };

        let acp_usage = acp_usage_from_agent_usage(&usage);

        assert_eq!(acp_usage.input_tokens, 123);
        assert_eq!(acp_usage.output_tokens, 45);
        assert_eq!(acp_usage.total_tokens, 168);
        assert_eq!(acp_usage.thought_tokens, None);
        assert_eq!(acp_usage.cached_read_tokens, None);
    }

    #[tokio::test]
    async fn usage_command_reports_actual_session_usage_without_estimated_cost() {
        let _guard = env_test_lock();
        hermes_core::credits::clear_last_nous_credits_state();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![
                hermes_core::Message::user("measure usage"),
                hermes_core::Message::assistant("measured"),
            ],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(hermes_core::UsageStats {
                prompt_tokens: 12,
                completion_tokens: 3,
                total_tokens: 15,
                estimated_cost: Some(0.0123),
            }),
            interrupted: false,
            session_cost_usd: Some(0.0123),
            session_started_hooks_fired: false,
        });

        let result = handle_slash_command(&mut app, "/usage", &[])
            .await
            .expect("usage command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Last response: 12 prompt / 3 completion / 15 total tokens"));
        assert!(output.contains("Actual session: 12 prompt / 3 completion / 15 total tokens"));
        assert!(!output.contains("Actual cost"));
        assert!(!output.contains("$0.0123"));
        hermes_core::credits::clear_last_nous_credits_state();
    }

    #[tokio::test]
    async fn usage_command_includes_last_nous_credits_state() {
        let _guard = env_test_lock();
        hermes_core::credits::clear_last_nous_credits_state();
        hermes_core::credits::capture_nous_credits_from_pairs([
            ("x-nous-credits-version", "1"),
            ("x-nous-credits-remaining-micros", "12000000"),
            ("x-nous-credits-remaining-usd", "12.00"),
            ("x-nous-credits-subscription-micros", "5000000"),
            ("x-nous-credits-subscription-usd", "5.00"),
            ("x-nous-credits-subscription-limit-micros", "10000000"),
            ("x-nous-credits-subscription-limit-usd", "10.00"),
            ("x-nous-credits-rollover-micros", "1000000"),
            ("x-nous-credits-purchased-micros", "7000000"),
            ("x-nous-credits-purchased-usd", "7.00"),
            ("x-nous-credits-denominator-kind", "subscription_cap"),
            ("x-nous-credits-paid-access", "true"),
        ])
        .expect("capture credits");
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/usage", &[])
            .await
            .expect("usage command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Nous credits"));
        assert!(output.contains("Subscription: 50% remaining (50% used)"));
        assert!(output.contains("Total usable: 12.00"));
        hermes_core::credits::clear_last_nous_credits_state();
    }

    #[tokio::test]
    async fn billing_command_renders_billing_surface_when_logged_out() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _real_home_guard = EnvVarGuard::set("HOME", tmp.path());
        let _auth_file_guard = EnvVarGuard::set("HERMES_AUTH_FILE", tmp.path().join("auth.json"));
        let _nous_oauth_guard =
            EnvVarGuard::set("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json"));
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/billing", &[])
            .await
            .expect("billing command");

        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Nous billing"));
        assert!(output.contains("Not logged into Nous Portal"));
        assert!(output.contains("Manage on portal:"));
    }

    #[test]
    fn test_acp_setup_browser_dependency_checks_forward_yes_flag() {
        let mut calls = Vec::new();
        let checks = acp_setup_browser_dependency_checks(true, |command| {
            calls.push(command.to_string());
            true
        })
        .expect("dependencies should pass");

        assert_eq!(calls, vec!["node", "agent-browser"]);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].dependency, "node");
        assert_eq!(checks[1].dependency, "browser");
        assert!(checks.iter().all(|check| check.available));
        assert!(checks.iter().all(|check| !check.interactive));
    }

    #[test]
    fn command_on_path_prefers_managed_node_before_path() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _path_guard = EnvVarGuard::set("PATH", "");
        write_test_executable(&tmp.path().join("node").join("bin").join("node"));

        assert!(command_on_path("node"));
    }

    #[test]
    fn whatsapp_bridge_start_command_prefers_managed_npx() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _path_guard = EnvVarGuard::set("PATH", "");
        let npx = tmp.path().join("node").join("bin").join("npx");
        write_test_executable(&npx);

        let command = whatsapp_bridge_start_command();

        assert_ne!(command, "npx hermes-whatsapp-bridge");
        assert!(command.contains("npx"));
        assert!(command.contains("hermes-whatsapp-bridge"));
    }

    #[test]
    fn test_acp_setup_browser_dependency_checks_stops_on_node_failure() {
        let mut calls = Vec::new();
        let err = acp_setup_browser_dependency_checks(false, |command| {
            calls.push(command.to_string());
            command != "node"
        })
        .expect_err("node failure should stop setup-browser");

        assert_eq!(calls, vec!["node"]);
        assert!(err.to_string().contains("node"));
    }
