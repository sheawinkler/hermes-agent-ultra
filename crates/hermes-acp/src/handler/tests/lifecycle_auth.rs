#[tokio::test]
async fn acp_mcp_server_params_convert_to_hermes_configs() {
    let params = json!({
        "mcpServers": [
            {
                "type": "stdio",
                "name": "bad/name",
                "command": "/bin/mcp",
                "args": ["--serve"],
                "env": [{"name": "MCP_KEY", "value": "v"}]
            },
            {
                "type": "http",
                "name": "remote.server",
                "url": "https://example.test/mcp",
                "headers": [{"name": "Authorization", "value": "Bearer test-token"}]
            }
        ]
    });
    let servers = acp_mcp_servers_from_params(params.as_object());
    assert_eq!(servers.len(), 2);

    let (stdio_name, stdio_config) =
        acp_mcp_server_to_hermes_config(&servers[0]).expect("stdio config");
    assert_eq!(stdio_name, "bad_name");
    assert_eq!(stdio_config.command.as_deref(), Some("/bin/mcp"));
    assert_eq!(stdio_config.args, vec!["--serve".to_string()]);
    assert_eq!(
        stdio_config.env.get("MCP_KEY").map(String::as_str),
        Some("v")
    );

    let (http_name, http_config) =
        acp_mcp_server_to_hermes_config(&servers[1]).expect("http config");
    assert_eq!(http_name, "remote_server");
    assert_eq!(http_config.url.as_deref(), Some("https://example.test/mcp"));
    let token = http_config
        .auth_provider
        .as_ref()
        .expect("bearer auth")
        .get_token()
        .await
        .expect("token");
    assert_eq!(token, "test-token");
}

#[test]
fn acp_enabled_toolsets_include_explicit_mcp_toolsets() {
    let expanded = expand_acp_enabled_toolsets(
        vec!["hermes-acp".to_string(), "hermes-acp".to_string()],
        vec!["srv".to_string(), "remote.server".to_string()],
    );
    assert_eq!(
        expanded,
        vec![
            "hermes-acp".to_string(),
            "mcp-srv".to_string(),
            "mcp-remote_server".to_string()
        ]
    );
}

#[tokio::test]
async fn tools_list_and_slash_tools_include_registered_mcp_tools() {
    let handler = make_handler();
    let schema = tool_schema("mcp_srv_ping", "MCP ping", JsonSchema::new("object"));
    handler.tool_registry().register(
        "mcp_srv_ping",
        "mcp-srv",
        schema.clone(),
        Arc::new(NoopTool { schema }),
        Arc::new(|| true),
        Vec::new(),
        true,
        "MCP ping",
        "mcp",
        None,
    );

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools.list".into(),
            params: None,
        })
        .await;
    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert!(tools
        .iter()
        .any(|tool| { tool["name"] == "mcp_srv_ping" && tool["description"] == "MCP ping" }));

    let slash = handler
        .handle_slash_command("/tools json", "session-id")
        .expect("tools slash response");
    assert!(slash.contains("mcp_srv_ping"));
    assert!(slash.contains("MCP ping"));
}

#[tokio::test]
async fn test_initialize() {
    let handler = make_handler();
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    assert!(resp.result.is_some());
    let result = resp.result.unwrap();
    assert_eq!(result["agentInfo"]["name"], "hermes-agent");
}

#[tokio::test]
async fn test_initialize_uses_acp_wire_aliases() {
    let handler = make_handler();
    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        })
        .await;
    let result = resp.result.unwrap();

    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentInfo"]["name"], "hermes-agent");
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["image"],
        true
    );
    assert_eq!(
        result["agentCapabilities"]["sessionCapabilities"]["fork"],
        true
    );
    assert_eq!(
        result["agentCapabilities"]["sessionCapabilities"]["resume"],
        true
    );
    assert!(result.get("protocol_version").is_none());
    assert!(result.get("agent_info").is_none());
    assert!(result["agentCapabilities"].get("load_session").is_none());
}

#[tokio::test]
async fn test_initialize_advertises_provider_and_terminal_auth_methods() {
    let handler = make_handler_with_auth_provider(Some("openrouter"));
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    let result = resp.result.unwrap();
    let methods = result["authMethods"].as_array().expect("auth methods");

    assert_eq!(methods[0]["id"], "openrouter");
    assert_eq!(methods[0]["name"], "openrouter runtime credentials");
    let terminal = methods
        .iter()
        .find(|method| method["id"] == TERMINAL_SETUP_AUTH_METHOD_ID)
        .expect("terminal setup auth method");
    assert_eq!(terminal["type"], "terminal");
    assert_eq!(terminal["args"], json!(["--setup"]));
}

#[tokio::test]
async fn test_initialize_advertises_terminal_setup_auth_when_no_provider() {
    let handler = make_handler_with_auth_provider(None);
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    let result = resp.result.unwrap();

    assert_eq!(
        result["authMethods"],
        json!([{
            "args": ["--setup"],
            "description": "Open Hermes' interactive model/provider setup in a terminal. Use this when Hermes has not been configured on this machine yet.",
            "id": TERMINAL_SETUP_AUTH_METHOD_ID,
            "name": "Configure Hermes provider",
            "type": "terminal",
        }])
    );
}

#[tokio::test]
async fn test_authenticate_accepts_matching_method_id_case_insensitively() {
    let handler = make_handler_with_auth_provider(Some("openrouter"));
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "authenticate".into(),
        params: Some(json!({"method_id": "OpenRouter"})),
    };

    let resp = handler.handle_request(req).await;
    assert_eq!(resp.result, Some(json!({})));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn test_authenticate_rejects_mismatched_method_id() {
    let handler = make_handler_with_auth_provider(Some("openrouter"));
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "authenticate".into(),
        params: Some(json!({"method_id": "totally-invalid-method"})),
    };

    let resp = handler.handle_request(req).await;
    assert_eq!(resp.result, Some(Value::Null));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn test_authenticate_accepts_terminal_setup_after_provider_configured() {
    let handler = make_handler_with_auth_provider(Some("openrouter"));
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "authenticate".into(),
        params: Some(json!({"method_id": TERMINAL_SETUP_AUTH_METHOD_ID})),
    };

    let resp = handler.handle_request(req).await;
    assert_eq!(resp.result, Some(json!({})));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn test_authenticate_rejects_terminal_setup_without_provider() {
    let handler = make_handler_with_auth_provider(None);
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "authenticate".into(),
        params: Some(json!({"method_id": TERMINAL_SETUP_AUTH_METHOD_ID})),
    };

    let resp = handler.handle_request(req).await;
    assert_eq!(resp.result, Some(Value::Null));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn test_session_lifecycle() {
    let handler = make_handler();

    // Create session
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "session/new".into(),
        params: Some(json!({"cwd": "/tmp"})),
    };
    let resp = handler.handle_request(req).await;
    let session_id = resp.result.unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    // List sessions
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "session/list".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    let sessions = resp.result.unwrap()["sessions"].as_array().unwrap().clone();
    assert_eq!(sessions.len(), 1);

    // Fork session
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "session/fork".into(),
        params: Some(json!({"session_id": session_id, "cwd": "/other"})),
    };
    let resp = handler.handle_request(req).await;
    assert!(resp.result.is_some());
}

#[tokio::test]
async fn test_session_methods_accept_camel_case_wire_fields() {
    let handler = make_handler();
    let created = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "/tmp"})),
        })
        .await
        .result
        .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();

    let loaded = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/load".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/work"})),
        })
        .await;
    assert!(loaded.error.is_none());

    let model = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/set_model".into(),
            params: Some(json!({"sessionId": session_id, "modelId": "nous:gpt-5.4"})),
        })
        .await;
    assert!(model.error.is_none());

    let mode = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "session/set_mode".into(),
            params: Some(json!({"sessionId": session_id, "modeId": "code"})),
        })
        .await;
    assert!(mode.error.is_none());

    let config = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(5)),
            method: "session/set_config".into(),
            params: Some(json!({
                "sessionId": session_id,
                "configId": "approval_mode",
                "value": "auto"
            })),
        })
        .await
        .result
        .unwrap();
    assert_eq!(config["configOptions"][0]["configId"], "approval_mode");

    let stable_config = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(6)),
            method: "session/set_config_option".into(),
            params: Some(json!({
                "sessionId": session_id,
                "configId": "sandbox",
                "value": "workspace-write"
            })),
        })
        .await
        .result
        .unwrap();
    assert_eq!(stable_config["configOptions"][0]["configId"], "sandbox");

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.cwd, "/work");
    assert_eq!(state.model.as_deref(), Some("nous:gpt-5.4"));
    assert_eq!(state.mode.as_deref(), Some("code"));
    assert_eq!(
        state
            .config_options
            .get("approval_mode")
            .map(String::as_str),
        Some("auto")
    );
    assert_eq!(
        state.config_options.get("sandbox").map(String::as_str),
        Some("workspace-write")
    );
}

#[tokio::test]
async fn test_session_title_rpc_updates_live_session_and_persists() {
    let persisted = Arc::new(std::sync::Mutex::new(Vec::<SessionState>::new()));
    let persisted_for_cb = persisted.clone();
    let session_manager = Arc::new(SessionManager::new().with_persist_callback(move |state| {
        persisted_for_cb
            .lock()
            .expect("persisted lock")
            .push(state.clone());
    }));
    let handler = HermesAcpHandler::new(
        session_manager,
        Arc::new(EventSink::default()),
        Arc::new(PermissionStore::new()),
    );
    let state = handler.session_manager.create_session("/runtime-only");
    let session_id = state.session_id;
    handler.session_manager.set_history(
        &session_id,
        vec![json!({"role": "user", "content": "fallback history title"})],
    );
    persisted.lock().expect("persisted lock").clear();

    let response = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session.title".into(),
            params: Some(json!({
                "session_id": session_id,
                "title": "  My branch  "
            })),
        })
        .await;

    assert!(response.error.is_none());
    let result = response.result.expect("title result");
    assert_eq!(result["sessionId"], session_id);
    assert_eq!(result["session_id"], session_id);
    assert_eq!(result["title"], "My branch");

    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.title.as_deref(), Some("My branch"));

    {
        let persisted = persisted.lock().expect("persisted lock");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].session_id, session_id);
        assert_eq!(persisted[0].title.as_deref(), Some("My branch"));
    }

    let events = handler.event_sink.drain_for_session(&session_id);
    let info_updates = events
        .iter()
        .filter(|event| event.kind == AcpEventKind::SessionInfoUpdate)
        .collect::<Vec<_>>();
    assert_eq!(info_updates.len(), 1);
    assert_eq!(info_updates[0].title.as_deref(), Some("My branch"));
    let content = info_updates[0]
        .content
        .as_ref()
        .expect("session info content");
    assert_eq!(content["sessionId"], session_id);
    assert_eq!(content["session_id"], session_id);
    assert_eq!(content["cwd"], "/runtime-only");
    assert_eq!(content["title"], "My branch");

    let listed = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/list".into(),
            params: Some(json!({"cwd": "/runtime-only"})),
        })
        .await
        .result
        .expect("list result");
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["title"], "My branch");
}

#[tokio::test]
async fn test_session_title_rpc_rejects_empty_and_missing_session() {
    let handler = make_handler();
    let session_id = create_session(&handler).await;

    let empty = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/title".into(),
            params: Some(json!({"sessionId": session_id, "title": "   "})),
        })
        .await;
    assert_eq!(empty.error.as_ref().map(|err| err.code), Some(-32602));
    assert_eq!(
        handler
            .session_manager
            .get_session(&session_id)
            .unwrap()
            .title,
        None
    );

    let missing = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session.title".into(),
            params: Some(json!({"sessionId": "missing", "title": "Name"})),
        })
        .await;
    assert_eq!(missing.error.as_ref().map(|err| err.code), Some(-32602));
    assert!(missing
        .error
        .as_ref()
        .is_some_and(|err| err.message.contains("Session not found")));
}

#[tokio::test]
async fn test_session_profile_home_metadata_flows_through_acp_lifecycle() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handler = make_handler().with_prompt_executor(Arc::new(ProfileRecordingPromptExecutor {
        seen: seen.clone(),
    }));

    let created = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({
                "cwd": "/tmp",
                "profile": "work",
                "profileHome": "/profiles/work"
            })),
        })
        .await
        .result
        .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.profile.as_deref(), Some("work"));
    assert_eq!(state.home.as_deref(), Some("/profiles/work"));

    let listed = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/list".into(),
            params: Some(json!({"profile": "work"})),
        })
        .await
        .result
        .unwrap();
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["profile"], "work");
    assert_eq!(sessions[0]["home"], "/profiles/work");

    let loaded = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/load".into(),
            params: Some(json!({
                "sessionId": session_id,
                "cwd": "/repo",
                "profile": "research",
                "homeDir": "/profiles/research"
            })),
        })
        .await;
    assert!(loaded.error.is_none());
    let state = handler.session_manager.get_session(&session_id).unwrap();
    assert_eq!(state.cwd, "/repo");
    assert_eq!(state.profile.as_deref(), Some("research"));
    assert_eq!(state.home.as_deref(), Some("/profiles/research"));

    let prompt = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "prompt".into(),
            params: Some(json!({
                "sessionId": session_id,
                "prompt": "who owns this profile?"
            })),
        })
        .await;
    assert!(prompt.error.is_none());
    assert_eq!(
        seen.lock().expect("profile recorder lock").as_slice(),
        &[(
            Some("research".to_string()),
            Some("/profiles/research".to_string())
        )]
    );

    let forked = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(5)),
            method: "session/fork".into(),
            params: Some(json!({
                "sessionId": session_id,
                "cwd": "/fork",
                "profile": "scratch",
                "home": "/profiles/scratch"
            })),
        })
        .await
        .result
        .unwrap();
    let forked_id = forked["sessionId"].as_str().unwrap().to_string();
    let forked_state = handler.session_manager.get_session(&forked_id).unwrap();
    assert_eq!(forked_state.profile.as_deref(), Some("scratch"));
    assert_eq!(forked_state.home.as_deref(), Some("/profiles/scratch"));

    let invalid = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(6)),
            method: "session/resume".into(),
            params: Some(json!({
                "sessionId": "missing",
                "profile": "../bad"
            })),
        })
        .await;
    assert!(invalid.error.is_some());
}

fn assert_available_commands_update(events: Vec<AcpEvent>, expected_session_id: &str) {
    assert_eq!(events.len(), 2);
    let event = &events[0];
    assert_eq!(event.kind, AcpEventKind::AvailableCommandsUpdate);
    assert_eq!(event.session_id, expected_session_id);
    assert_eq!(
        event.session_update.as_deref(),
        Some("available_commands_update")
    );
    let commands = event
        .available_commands
        .as_ref()
        .expect("available commands");
    assert!(commands.iter().any(|command| command.name == "help"));
    assert!(commands.iter().any(|command| command.name == "version"));
    let model = commands
        .iter()
        .find(|command| command.name == "model")
        .expect("model command");
    assert_eq!(model.input_hint.as_deref(), Some("model name to switch to"));
    assert!(commands.iter().any(|command| command.name == "queue"));
    assert!(commands.iter().any(|command| command.name == "steer"));

    let usage = &events[1];
    assert_eq!(usage.kind, AcpEventKind::UsageUpdate);
    assert_eq!(usage.session_id, expected_session_id);
    assert_eq!(usage.session_update.as_deref(), Some("usage_update"));
    assert!(usage.size.unwrap_or_default() > 0);
    assert!(usage.used.is_some());
}

#[tokio::test]
async fn test_session_lifecycle_advertises_available_commands() {
    let handler = make_handler();

    let created = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "/tmp"})),
        })
        .await
        .result
        .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    assert_available_commands_update(
        handler.event_sink.drain_for_session(&session_id),
        &session_id,
    );

    let loaded = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "session/load".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/work"})),
        })
        .await;
    assert!(loaded.error.is_none());
    assert_available_commands_update(
        handler.event_sink.drain_for_session(&session_id),
        &session_id,
    );

    let resumed = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "session/resume".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/work"})),
        })
        .await;
    assert!(resumed.error.is_none());
    let resume_events = handler.event_sink.drain_for_session(&session_id);
    assert_eq!(resume_events[0].kind, AcpEventKind::SessionInfoUpdate);
    assert_eq!(
        resume_events[0].content.as_ref().expect("session info")["cwd"],
        "/work"
    );
    assert_available_commands_update(resume_events[1..].to_vec(), &session_id);

    let forked = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "session/fork".into(),
            params: Some(json!({"sessionId": session_id, "cwd": "/fork"})),
        })
        .await
        .result
        .unwrap();
    let forked_session_id = forked["sessionId"].as_str().unwrap().to_string();
    assert_available_commands_update(
        handler.event_sink.drain_for_session(&forked_session_id),
        &forked_session_id,
    );
}
