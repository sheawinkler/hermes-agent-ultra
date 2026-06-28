#[tokio::test]
async fn test_load_session_replays_persisted_history_to_client() {
    let handler = make_handler();
    let created = handler.session_manager.create_session("/tmp");
    let session_id = created.session_id;
    handler.session_manager.set_history(
        &session_id,
        vec![
            json!({"role": "system", "content": "hidden"}),
            json!({"role": "user", "content": "what controls slash commands?"}),
            json!({
                "role": "assistant",
                "reasoning_content": "Look up the ACP command table first.",
                "content": [{"type": "text", "text": "The advertised commands do."}],
                "tool_calls": [{
                    "id": "tc-read",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"/tmp/a.txt\"}"
                    }
                }]
            }),
            json!({"role": "tool", "tool_call_id": "tc-read", "content": "file contents"}),
        ],
    );

    let loaded = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/load".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/tmp"})),
        })
        .await;
    assert!(loaded.error.is_none());

    let events = handler.event_sink.drain_for_session(&session_id);
    let kinds = events
        .iter()
        .map(|event| event.kind.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            AcpEventKind::UserMessageChunk,
            AcpEventKind::AgentThoughtChunk,
            AcpEventKind::AgentMessageChunk,
            AcpEventKind::ToolCallStart,
            AcpEventKind::ToolCallComplete,
            AcpEventKind::AvailableCommandsUpdate,
            AcpEventKind::UsageUpdate,
        ]
    );
    assert_eq!(
        events[0].session_update.as_deref(),
        Some("user_message_chunk")
    );
    assert_eq!(
        events[0].content.as_ref().unwrap()["text"],
        "what controls slash commands?"
    );
    assert_eq!(
        events[1].session_update.as_deref(),
        Some("agent_thought_chunk")
    );
    assert_eq!(
        events[1].content.as_ref().unwrap()["text"],
        "Look up the ACP command table first."
    );
    assert_eq!(
        events[2].session_update.as_deref(),
        Some("agent_message_chunk")
    );
    assert_eq!(events[3].tool_call_id.as_deref(), Some("tc-read"));
    assert_eq!(events[3].tool_name.as_deref(), Some("read_file"));
    assert_eq!(events[3].arguments.as_ref().unwrap()["path"], "/tmp/a.txt");
    assert_eq!(events[4].result.as_deref(), Some("file contents"));
    let replay_user_json = serde_json::to_value(&events[0]).unwrap();
    let replay_agent_json = serde_json::to_value(&events[2]).unwrap();
    assert!(replay_user_json.get("messageId").is_none());
    assert!(replay_agent_json.get("messageId").is_none());
}

#[tokio::test]
async fn test_resume_session_replays_reasoning_only_turn() {
    let handler = make_handler();
    let created = handler.session_manager.create_session("/tmp");
    let session_id = created.session_id;
    handler.session_manager.set_history(
        &session_id,
        vec![json!({
            "role": "assistant",
            "reasoning": [{"text": "Reasoning persisted without assistant text."}],
            "content": ""
        })],
    );

    let resumed = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/resume".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/tmp"})),
        })
        .await;
    assert!(resumed.error.is_none());

    let events = handler.event_sink.drain_for_session(&session_id);
    assert_eq!(events[0].kind, AcpEventKind::SessionInfoUpdate);
    assert_eq!(
        events[0].content.as_ref().expect("session info")["cwd"],
        "/tmp"
    );
    assert_eq!(events[1].kind, AcpEventKind::AgentThoughtChunk);
    assert_eq!(
        events[1].content.as_ref().unwrap()["text"],
        "Reasoning persisted without assistant text."
    );
    assert_eq!(
        events[events.len() - 2].kind,
        AcpEventKind::AvailableCommandsUpdate
    );
    assert_eq!(events.last().unwrap().kind, AcpEventKind::UsageUpdate);
}

#[tokio::test]
async fn test_resume_session_updates_runtime_metadata_without_prompt_execution() {
    let handler = HermesAcpHandler::new(
        Arc::new(SessionManager::new()),
        Arc::new(EventSink::default()),
        Arc::new(PermissionStore::new()),
    )
    .with_prompt_executor(Arc::new(ForbiddenPromptExecutor));
    let state = handler.session_manager.create_session_with_meta(
        "/old",
        SessionMetaUpdate {
            model: Some("dynamic".to_string()),
            provider: Some("openrouter".to_string()),
            api_mode: Some("chat".to_string()),
            base_url: Some("https://old.example/v1".to_string()),
            profile: Some("work".to_string()),
            home: Some("/profiles/work".to_string()),
            title: Some("Old workspace".to_string()),
            ..SessionMetaUpdate::default()
        },
    );
    let session_id = state.session_id;
    handler.session_manager.set_history(
        &session_id,
        vec![json!({"role": "user", "content": "replay me"})],
    );

    let resumed = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/resume".into(),
            params: Some(json!({
                "sessionId": session_id,
                "cwd": "/workspace/repo",
                "modelId": "dynamic",
                "provider": "openrouter",
                "apiMode": "responses",
                "baseUrl": "https://router.example/v1",
                "profile": "research",
                "homeDir": "/profiles/research",
                "title": "  Active workspace  ",
                "reasoningEffort": "medium",
                "serviceTier": "auto"
            })),
        })
        .await;
    assert!(resumed.error.is_none());

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.cwd, "/workspace/repo");
    assert_eq!(state.model.as_deref(), Some("dynamic"));
    assert_eq!(state.provider.as_deref(), Some("openrouter"));
    assert_eq!(state.api_mode.as_deref(), Some("responses"));
    assert_eq!(state.base_url.as_deref(), Some("https://router.example/v1"));
    assert_eq!(state.profile.as_deref(), Some("research"));
    assert_eq!(state.home.as_deref(), Some("/profiles/research"));
    assert_eq!(state.title.as_deref(), Some("Active workspace"));
    assert_eq!(
        state
            .config_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("medium")
    );
    assert_eq!(
        state.config_options.get("service_tier").map(String::as_str),
        Some("auto")
    );

    let events = handler.event_sink.drain_for_session(&session_id);
    assert_eq!(events[0].kind, AcpEventKind::SessionInfoUpdate);
    assert_eq!(events[0].title.as_deref(), Some("Active workspace"));
    let info = events[0].content.as_ref().expect("session info content");
    assert_eq!(info["sessionId"], session_id);
    assert_eq!(info["session_id"], session_id);
    assert_eq!(info["cwd"], "/workspace/repo");
    assert_eq!(info["model"], "dynamic");
    assert_eq!(info["provider"], "openrouter");
    assert_eq!(info["apiMode"], "responses");
    assert_eq!(info["baseUrl"], "https://router.example/v1");
    assert_eq!(info["profile"], "research");
    assert_eq!(info["home"], "/profiles/research");
    assert_eq!(info["title"], "Active workspace");
    assert!(events
        .iter()
        .any(|event| event.kind == AcpEventKind::UserMessageChunk));
    assert!(events
        .iter()
        .any(|event| event.kind == AcpEventKind::AvailableCommandsUpdate));
}

#[tokio::test]
async fn test_load_session_replays_native_plan_for_persisted_todo_tool() {
    let handler = make_handler();
    let created = handler.session_manager.create_session("/tmp");
    let session_id = created.session_id;
    let todo_result = r#"{"todos":[{"id":"ship","content":"Ship it","status":"in_progress"}]}"#;
    handler.session_manager.set_history(
        &session_id,
        vec![
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_todo_1",
                    "function": {
                        "name": "todo",
                        "arguments": "{\"todos\":[{\"id\":\"ship\",\"content\":\"Ship it\",\"status\":\"in_progress\"}]}"
                    }
                }]
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_todo_1",
                "content": todo_result,
            }),
        ],
    );

    let loaded = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/load".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/tmp"})),
        })
        .await;
    assert!(loaded.error.is_none());

    let events = handler.event_sink.drain_for_session(&session_id);
    let kinds = events
        .iter()
        .map(|event| event.kind.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            AcpEventKind::ToolCallStart,
            AcpEventKind::ToolCallComplete,
            AcpEventKind::PlanUpdate,
            AcpEventKind::AvailableCommandsUpdate,
            AcpEventKind::UsageUpdate,
        ]
    );
    assert_eq!(events[2].session_update.as_deref(), Some("plan"));
    let entries = events[2].entries.as_ref().expect("plan entries");
    assert_eq!(entries[0].content, "Ship it");
    assert_eq!(entries[0].status, "in_progress");
}

#[tokio::test]
async fn test_set_session_model_preserves_provider_route_metadata() {
    let handler = make_handler();
    let state = handler.session_manager.create_session("/workspace");
    let session_id = state.session_id;
    handler
        .session_manager
        .update_session_meta(
            &session_id,
            SessionMetaUpdate {
                model: Some("openrouter:anthropic/claude-sonnet-4".to_string()),
                provider: Some("openrouter".to_string()),
                api_mode: Some("responses".to_string()),
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
                ..SessionMetaUpdate::default()
            },
        )
        .expect("session exists");

    let response = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/set_model".into(),
            params: Some(json!({
                "sessionId": session_id,
                "modelId": "openai/gpt-4.1"
            })),
        })
        .await;
    assert!(response.error.is_none());

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.model.as_deref(), Some("openai/gpt-4.1"));
    assert_eq!(state.provider.as_deref(), Some("openrouter"));
    assert_eq!(state.api_mode.as_deref(), Some("responses"));
    assert_eq!(
        state.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
}

#[tokio::test]
async fn test_context_slash_command_includes_usage_and_threshold() {
    let handler = make_handler();
    let state = handler.session_manager.create_session("/workspace");
    let session_id = state.session_id;
    handler
        .session_manager
        .update_model(&session_id, "gpt-4o")
        .expect("session exists");
    handler.session_manager.set_history(
        &session_id,
        vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ],
    );

    let response = handler
        .handle_slash_command("/context", &session_id)
        .expect("context response");

    assert!(response.contains("Conversation: 2 messages"));
    assert!(response.contains("user: 1, assistant: 1"));
    assert!(response.contains("Context usage: ~"));
    assert!(response.contains("/ 128,000 tokens"));
    assert!(response.contains("Compression: ~"));
    assert!(response.contains("tokens until threshold (~102,400, 80%)."));
}

#[tokio::test]
async fn test_version_slash_command_uses_shared_version_label() {
    let handler = make_handler();
    let state = handler.session_manager.create_session("/workspace");

    let response = handler
        .handle_slash_command("/version", &state.session_id)
        .expect("version response");

    assert_eq!(response, hermes_core::version::version_label());
}

#[tokio::test]
async fn test_list_sessions_filters_paginates_and_uses_wire_metadata() {
    let handler = make_handler();
    let keep = handler.session_manager.create_session("/keep");
    handler.session_manager.set_history(
        &keep.session_id,
        vec![json!({"role": "user", "content": "Fix server wire format"})],
    );
    handler.session_manager.create_session("/drop");

    let filtered = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/list".into(),
            params: Some(json!({"cwd": "/keep"})),
        })
        .await
        .result
        .unwrap();
    let sessions = filtered["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], keep.session_id);
    assert_eq!(sessions[0]["title"], "Fix server wire format");
    assert!(sessions[0].get("updatedAt").is_some());
    assert!(sessions[0].get("historyLen").is_some());

    let pager = make_handler();
    for _ in 0..52 {
        pager.session_manager.create_session("/page");
    }
    let first_page = pager
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/list".into(),
            params: Some(json!({"cwd": "/page"})),
        })
        .await
        .result
        .unwrap();
    let first_sessions = first_page["sessions"].as_array().unwrap();
    assert_eq!(first_sessions.len(), 50);
    let cursor = first_page["nextCursor"].as_str().unwrap();

    let second_page = pager
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/list".into(),
            params: Some(json!({"cwd": "/page", "cursor": cursor})),
        })
        .await
        .result
        .unwrap();
    assert_eq!(second_page["sessions"].as_array().unwrap().len(), 2);
    assert!(second_page["nextCursor"].is_null());
}
