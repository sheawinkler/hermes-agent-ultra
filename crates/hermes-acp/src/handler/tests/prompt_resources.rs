#[tokio::test]
async fn test_prompt_slash_command() {
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

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id,
            "text": "/help",
        })),
    };
    let resp = handler.handle_request(req).await;
    assert_eq!(
        resp.result.unwrap()["stopReason"].as_str().unwrap(),
        "end_turn"
    );
}

#[tokio::test]
async fn test_prompt_accepts_content_alias_and_refuses_missing_session() {
    let handler = make_handler();
    let created = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        })
        .await
        .result
        .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();

    let prompt = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "sessionId": session_id,
                "content": [{"type": "text", "text": "ping"}],
            })),
        })
        .await
        .result
        .unwrap();
    assert_eq!(prompt["stopReason"], "end_turn");

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.history[0]["content"][0]["text"], "ping");

    let missing = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "prompt".into(),
            params: Some(json!({
                "sessionId": "missing",
                "content": [{"type": "text", "text": "ping"}],
            })),
        })
        .await;
    assert!(missing.error.is_none());
    assert_eq!(missing.result.unwrap()["stopReason"], "refusal");
}

#[tokio::test]
async fn test_prompt_resource_link_inlines_text_file() {
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

    let tmp_path = std::env::temp_dir().join(format!("hermes-acp-{}.txt", session_id));
    std::fs::write(&tmp_path, "trade-edge-notes").expect("write resource file");
    let file_uri = format!("file://{}", tmp_path.display());

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id.clone(),
            "prompt": [
                {"type": "text", "text": "review this"},
                {"type": "resource_link", "uri": file_uri, "name": "notes.txt", "mimeType": "text/plain"}
            ]
        })),
    };
    let resp = handler.handle_request(req).await;
    assert_eq!(
        resp.result.as_ref().unwrap()["stopReason"].as_str(),
        Some("end_turn")
    );
    let state = handler.session_manager.get_session(&session_id).unwrap();
    let user_content = state
        .history
        .iter()
        .find(|v| v.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|v| v.get("content"))
        .cloned()
        .unwrap_or(Value::Null);
    assert!(user_content.is_array());
    let flattened = user_content
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(flattened.contains("trade-edge-notes"));

    let _ = std::fs::remove_file(tmp_path);
}

#[tokio::test]
async fn test_prompt_with_image_and_slash_text_not_intercepted_as_command() {
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

    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "prompt".into(),
        params: Some(json!({
            "session_id": session_id.clone(),
            "prompt": [
                {"type": "text", "text": "/help"},
                {"type": "image", "url": "https://example.com/chart.png"}
            ]
        })),
    };
    let _ = handler.handle_request(req).await;

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert!(state.history.len() >= 2);
    let assistant_text = state
        .history
        .last()
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(assistant_text.contains("ACP session"));
}

#[tokio::test]
async fn test_prompt_forwards_acp_image_data_blocks_as_multimodal_content() {
    let handler = make_handler();
    let session_id = create_session(&handler).await;

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({
                "sessionId": session_id.clone(),
                "prompt": [
                    {"type": "text", "text": "What is in this image?"},
                    {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}
                ]
            })),
        })
        .await;
    assert!(resp.error.is_none());

    let state = handler.session_manager.get_session(&session_id).unwrap();
    let parts = state.history[0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "What is in this image?");
    assert_eq!(parts[1]["type"], "text");
    assert_eq!(parts[1]["text"], "[Attached image: image/png]");
    assert_eq!(parts[2]["type"], "image_url");
    assert_eq!(
        parts[2]["image_url"]["url"],
        "data:image/png;base64,aGVsbG8="
    );
}
