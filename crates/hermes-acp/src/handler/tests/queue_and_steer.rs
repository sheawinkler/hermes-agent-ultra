#[tokio::test]
async fn test_compact_slash_command_reduces_history() {
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

    let mut history = Vec::new();
    for i in 0..14 {
        history.push(json!({
            "role": if i % 2 == 0 { "user" } else { "assistant" },
            "content": format!("message {}", i),
        }));
    }
    handler.session_manager.set_history(&session_id, history);

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id.clone(),
            "text": "/compact",
        })),
    };
    let _ = handler.handle_request(req).await;

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert!(state.history.len() < 14);
}

#[tokio::test]
async fn test_help_lists_queue_and_steer_slash_commands() {
    let handler = make_handler();
    let session_id = create_session(&handler).await;

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/help",
            })),
        })
        .await;
    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");

    let events = handler.event_sink.drain_for_session(&session_id);
    let help_text = events
        .iter()
        .filter(|event| matches!(event.kind, AcpEventKind::MessageComplete))
        .filter_map(|event| event.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(help_text.contains("/queue"));
    assert!(help_text.contains("/steer"));
}

#[tokio::test]
async fn test_acp_queue_slash_command_adds_next_turn_without_running_now() {
    let executor = Arc::new(SteeringPromptExecutor::default());
    let handler = make_handler().with_prompt_executor(executor.clone());
    let session_id = create_session(&handler).await;

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/queue run the tests after this",
            })),
        })
        .await;

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.queued_prompts, vec!["run the tests after this"]);
    assert!(state.history.is_empty());
    assert!(executor.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_acp_prompt_drains_queued_turns_after_current_run() {
    let handler = HermesAcpHandler::new(
        Arc::new(SessionManager::new()),
        Arc::new(EventSink::default()),
        Arc::new(PermissionStore::new()),
    )
    .with_prompt_executor(Arc::new(EchoPromptExecutor));
    let session_id = create_session(&handler).await;
    handler
        .session_manager
        .push_queued_prompt(&session_id, "then run tests");

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "make the change",
            })),
        })
        .await;
    let result = resp.result.unwrap();
    assert_eq!(result["stopReason"], "end_turn");
    assert_eq!(result["usage"]["inputTokens"], 6);
    assert_eq!(result["usage"]["outputTokens"], 10);

    let state = handler.session_manager.get_session(&session_id).unwrap();
    let user_turns = state
        .history
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(user_turns, vec!["make the change", "then run tests"]);
    assert!(state.queued_prompts.is_empty());
}

#[tokio::test]
async fn test_acp_regular_prompt_queues_while_session_is_active() {
    let executor = Arc::new(SteeringPromptExecutor::default());
    let handler = make_handler().with_prompt_executor(executor.clone());
    let session_id = create_session(&handler).await;
    handler
        .session_manager
        .set_phase(&session_id, SessionPhase::Active);

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "follow up after current work",
            })),
        })
        .await;

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.queued_prompts, vec!["follow up after current work"]);
    assert!(state.history.is_empty());
    assert!(executor.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_acp_steer_slash_command_signals_active_executor_and_queues_guidance() {
    let executor = Arc::new(SteeringPromptExecutor::default());
    let handler = make_handler().with_prompt_executor(executor.clone());
    let session_id = create_session(&handler).await;
    handler
        .session_manager
        .set_phase(&session_id, SessionPhase::Active);

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/steer prefer the simpler fix",
            })),
        })
        .await;

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    assert_eq!(
        executor.steers.lock().unwrap().as_slice(),
        ["prefer the simpler fix"]
    );
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.queued_prompts, vec!["prefer the simpler fix"]);
    assert!(executor.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_acp_steer_on_idle_session_runs_as_regular_prompt() {
    let executor = Arc::new(SteeringPromptExecutor::default());
    let handler = make_handler().with_prompt_executor(executor.clone());
    let session_id = create_session(&handler).await;

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/steer summarize the README",
            })),
        })
        .await;

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    assert_eq!(
        executor.runs.lock().unwrap().as_slice(),
        ["summarize the README"]
    );
    assert!(executor.steers.lock().unwrap().is_empty());
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert!(state.queued_prompts.is_empty());
}

#[tokio::test]
async fn test_acp_steer_after_cancel_replays_interrupted_prompt_with_guidance() {
    let executor = Arc::new(SteeringPromptExecutor::default());
    let handler = make_handler().with_prompt_executor(executor.clone());
    let session_id = create_session(&handler).await;
    handler.session_manager.set_history(
        &session_id,
        vec![json!({"role": "user", "content": "write hi to a text file"})],
    );
    handler
        .session_manager
        .set_phase(&session_id, SessionPhase::Active);

    let cancel = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/cancel".into(),
            params: Some(json!({"session_id": session_id.clone()})),
        })
        .await;
    assert_eq!(cancel.result.unwrap()["cancelled"], true);

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "prompt".into(),
            params: Some(json!({
                "session_id": session_id.clone(),
                "text": "/steer write HELLO instead",
            })),
        })
        .await;

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    assert_eq!(
        executor.runs.lock().unwrap().as_slice(),
        ["write hi to a text file\n\nUser correction/guidance after interrupt: write HELLO instead"]
    );
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert!(state.interrupted_prompt_text.is_none());
    assert!(state.queued_prompts.is_empty());
}
