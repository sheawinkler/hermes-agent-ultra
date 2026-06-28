use super::*;
use crate::events::AcpEventKind;
use crate::session::SessionMetaUpdate;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use serde_json::json;

struct EchoPromptExecutor;

#[async_trait::async_trait]
impl AcpPromptExecutor for EchoPromptExecutor {
    async fn execute_prompt(
        &self,
        _session: &SessionState,
        user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        Ok(PromptExecutionOutput {
            response_text: format!("executor:{user_text}"),
            usage: Some(Usage {
                input_tokens: 3,
                output_tokens: 5,
                total_tokens: 8,
                thought_tokens: None,
                cached_read_tokens: None,
            }),
            total_turns: Some(2),
            events: Vec::new(),
        })
    }
}

struct NoopTool {
    schema: ToolSchema,
}

#[async_trait::async_trait]
impl ToolHandler for NoopTool {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Ok("ok".to_string())
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}

type SeenProfiles = Vec<(Option<String>, Option<String>)>;

struct ProfileRecordingPromptExecutor {
    seen: Arc<std::sync::Mutex<SeenProfiles>>,
}

#[async_trait::async_trait]
impl AcpPromptExecutor for ProfileRecordingPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &SessionState,
        user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        self.seen
            .lock()
            .expect("profile recorder lock")
            .push((session.profile.clone(), session.home.clone()));
        Ok(PromptExecutionOutput {
            response_text: format!("profiled:{user_text}"),
            usage: None,
            total_turns: Some(1),
            events: Vec::new(),
        })
    }
}

struct ToolEventPromptExecutor;

#[async_trait::async_trait]
impl AcpPromptExecutor for ToolEventPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &SessionState,
        _user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        Ok(PromptExecutionOutput {
            response_text: "done".to_string(),
            usage: None,
            total_turns: Some(1),
            events: vec![
                AcpEvent::tool_call_start(
                    &session.session_id,
                    "tc-read",
                    "read_file",
                    Some(json!({"path": "/tmp/a.txt"})),
                ),
                AcpEvent::tool_call_complete(
                    &session.session_id,
                    "tc-read",
                    "read_file",
                    Some("contents".to_string()),
                ),
            ],
        })
    }
}

struct StreamingPromptExecutor;

#[async_trait::async_trait]
impl AcpPromptExecutor for StreamingPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &SessionState,
        _user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        Ok(PromptExecutionOutput {
            response_text: "streamed answer".to_string(),
            usage: None,
            total_turns: Some(1),
            events: vec![AcpEvent::message_delta(
                &session.session_id,
                "streamed answer",
            )],
        })
    }
}

struct ForbiddenPromptExecutor;

#[async_trait::async_trait]
impl AcpPromptExecutor for ForbiddenPromptExecutor {
    async fn execute_prompt(
        &self,
        _session: &SessionState,
        _user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        panic!("session resume must not execute or build a prompt executor");
    }
}

#[derive(Default)]
struct SteeringPromptExecutor {
    steers: std::sync::Mutex<Vec<String>>,
    runs: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AcpPromptExecutor for SteeringPromptExecutor {
    async fn execute_prompt(
        &self,
        _session: &SessionState,
        user_text: &str,
        _history: &[Value],
    ) -> Result<PromptExecutionOutput, String> {
        self.runs.lock().unwrap().push(user_text.to_string());
        Ok(PromptExecutionOutput {
            response_text: format!("ran:{user_text}"),
            usage: None,
            total_turns: Some(1),
            events: Vec::new(),
        })
    }

    fn steer_prompt(&self, _session: &SessionState, guidance: &str) -> Result<bool, String> {
        self.steers.lock().unwrap().push(guidance.to_string());
        Ok(true)
    }
}

fn make_handler() -> HermesAcpHandler {
    HermesAcpHandler::new(
        Arc::new(SessionManager::new()),
        Arc::new(EventSink::default()),
        Arc::new(PermissionStore::new()),
    )
}

async fn create_session(handler: &HermesAcpHandler) -> String {
    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "session/new".into(),
            params: Some(json!({"cwd": "."})),
        })
        .await;
    resp.result.unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn make_handler_with_auth_provider(provider: Option<&'static str>) -> HermesAcpHandler {
    make_handler().with_auth_provider_resolver(Arc::new(move || provider.map(str::to_string)))
}

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

#[tokio::test]
async fn test_unknown_method() {
    let handler = make_handler();
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "foo.bar".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, -32601);
}

#[tokio::test]
async fn test_legacy_create_conversation() {
    let handler = DefaultAcpHandler::default();
    let req = AcpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "conversation.create".into(),
        params: None,
    };
    let resp = handler.handle_request(req).await;
    assert!(resp.result.is_some());
    assert!(resp.result.unwrap().get("conversation_id").is_some());
}

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
