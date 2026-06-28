#[tokio::test]
async fn test_list_tools_non_empty() {
    let handler = make_handler();
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools.list".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    let tools = resp
        .result
        .unwrap()
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(!tools.is_empty());
}

#[tokio::test]
async fn test_prompt_uses_custom_executor_and_records_usage() {
    let handler = HermesAcpHandler::new(
        Arc::new(SessionManager::new()),
        Arc::new(EventSink::default()),
        Arc::new(PermissionStore::new()),
    )
    .with_prompt_executor(Arc::new(EchoPromptExecutor));

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "session/new".into(),
        params: Some(json!({"cwd": "."})),
    };
    let resp = handler.handle_request(req).await;
    let session_id = resp.result.unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id.clone(),
            "text": "hello"
        })),
    };
    let resp = handler.handle_request(req).await;
    let usage = resp.result.unwrap()["usage"].clone();
    assert_eq!(usage["inputTokens"], 3);
    assert_eq!(usage["outputTokens"], 5);

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.total_prompt_tokens, 3);
    assert_eq!(state.total_completion_tokens, 5);
    assert_eq!(
        state
            .history
            .last()
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str()),
        Some("executor:hello")
    );

    let events = handler.event_sink.drain_for_session(&session_id);
    let info_updates = events
        .iter()
        .filter(|event| event.kind == AcpEventKind::SessionInfoUpdate)
        .collect::<Vec<_>>();
    assert_eq!(info_updates.len(), 1);
    assert_eq!(
        info_updates[0].session_update.as_deref(),
        Some("session_info_update")
    );
    assert_eq!(info_updates[0].title.as_deref(), Some("hello"));
    assert!(info_updates[0]
        .updated_at
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|value| value > 0));
}

#[tokio::test]
async fn test_prompt_does_not_duplicate_streamed_final_message() {
    let handler = make_handler().with_prompt_executor(Arc::new(StreamingPromptExecutor));
    let session_id = create_session(&handler).await;

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "sessionId": session_id.clone(),
                "text": "hello"
            })),
        })
        .await;
    assert!(resp.error.is_none());

    let events = handler.event_sink.drain_for_session(&session_id);
    let message_events = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                AcpEventKind::MessageDelta | AcpEventKind::MessageComplete
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(message_events.len(), 1);
    assert_eq!(message_events[0].kind, AcpEventKind::MessageDelta);
    assert_eq!(message_events[0].text.as_deref(), Some("streamed answer"));
}

#[tokio::test]
async fn test_prompt_enqueues_executor_tool_events() {
    let event_sink = Arc::new(EventSink::default());
    let handler = HermesAcpHandler::new(
        Arc::new(SessionManager::new()),
        event_sink.clone(),
        Arc::new(PermissionStore::new()),
    )
    .with_prompt_executor(Arc::new(ToolEventPromptExecutor));

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "session/new".into(),
        params: Some(json!({"cwd": "."})),
    };
    let resp = handler.handle_request(req).await;
    let session_id = resp.result.unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id.clone(),
            "text": "read file"
        })),
    };
    let resp = handler.handle_request(req).await;
    assert_eq!(
        resp.result.unwrap()["stopReason"].as_str(),
        Some("end_turn")
    );

    let events = event_sink.drain_for_session(&session_id);
    let tool_events: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                AcpEventKind::ToolCallStart | AcpEventKind::ToolCallComplete
            )
        })
        .collect();
    assert_eq!(tool_events.len(), 2);
    assert_eq!(tool_events[0].tool_call_id.as_deref(), Some("tc-read"));
    assert_eq!(tool_events[0].tool_name.as_deref(), Some("read_file"));
    assert_eq!(
        tool_events[0].arguments.as_ref().unwrap()["path"],
        "/tmp/a.txt"
    );
    assert_eq!(tool_events[1].tool_call_id.as_deref(), Some("tc-read"));
    assert_eq!(tool_events[1].result.as_deref(), Some("contents"));
}

#[tokio::test]
async fn test_set_session_fields_persist() {
    let handler = make_handler();

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "session/new".into(),
        params: Some(json!({"cwd": "."})),
    };
    let resp = handler.handle_request(req).await;
    let session_id = resp.result.unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let set_model = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "session/set_model".into(),
        params: Some(json!({"session_id": session_id, "model_id": "nous:gpt-5.4"})),
    };
    let _ = handler.handle_request(set_model).await;

    let set_mode = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "session/set_mode".into(),
        params: Some(json!({"session_id": session_id, "mode_id": "code"})),
    };
    let _ = handler.handle_request(set_mode).await;

    let set_cfg = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(4)),
        method: "session/set_config".into(),
        params: Some(json!({
            "session_id": session_id,
            "key": "temperature",
            "value": "0.1"
        })),
    };
    let _ = handler.handle_request(set_cfg).await;

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.model.as_deref(), Some("nous:gpt-5.4"));
    assert_eq!(state.mode.as_deref(), Some("code"));
    assert_eq!(
        state.config_options.get("temperature").map(String::as_str),
        Some("0.1")
    );
}
