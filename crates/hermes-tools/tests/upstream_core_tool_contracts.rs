use async_trait::async_trait;
use hermes_core::{
    tool_schema, AgentError, CommandOutput, JsonSchema, TerminalBackend, ToolError, ToolHandler,
    ToolSchema,
};
use hermes_tools::{
    ClarifyBackend, ClarifyHandler, CodeExecutionBackend, CronjobBackend, CronjobHandler,
    ExecuteCodeHandler, HaCallServiceHandler, HaGetStateHandler, HaListEntitiesHandler,
    HaListServicesHandler, HomeAssistantBackend, MemoryBackend, MemoryHandler, MessagingBackend,
    ProcessRegistryHandler, SendMessageHandler, SessionSearchBackend, SessionSearchHandler,
    ToolRegistry,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn assert_err_contains<T: std::fmt::Debug>(result: Result<T, ToolError>, expected: &str) {
    let err = result.expect_err("expected error");
    assert!(
        err.to_string().contains(expected),
        "expected error to contain {expected:?}, got {err}"
    );
}

#[derive(Default)]
struct CaptureClarifyBackend {
    calls: Mutex<Vec<(String, Option<Vec<String>>)>>,
}

#[async_trait]
impl ClarifyBackend for CaptureClarifyBackend {
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push((question.to_string(), choices.map(|items| items.to_vec())));
        Ok("answer".to_string())
    }
}

#[tokio::test]
async fn clarify_contract_trims_question_and_normalizes_choices() {
    let backend = Arc::new(CaptureClarifyBackend::default());
    let handler = ClarifyHandler::new(backend.clone());

    let answer = handler
        .execute(json!({
            "question": "  Pick one  ",
            "choices": [" alpha ", "", "beta", 3, true, "gamma", "delta"]
        }))
        .await
        .expect("clarify succeeds");

    assert_eq!(answer, "answer");
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [(
            "Pick one".to_string(),
            Some(vec![
                "alpha".to_string(),
                "beta".to_string(),
                "3".to_string(),
                "true".to_string()
            ])
        )]
    );
    assert_err_contains(
        handler.execute(json!({"question": "   "})).await,
        "cannot be empty",
    );
    assert_err_contains(
        handler
            .execute(json!({"question": "Pick?", "choices": "bad"}))
            .await,
        "must be an array",
    );
}

#[derive(Default)]
struct CaptureCodeBackend {
    calls: Mutex<Vec<CodeExecutionCall>>,
}

type CodeExecutionCall = (String, Option<String>, Option<u64>);

#[async_trait]
impl CodeExecutionBackend for CaptureCodeBackend {
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        self.calls.lock().expect("calls").push((
            code.to_string(),
            language.map(ToOwned::to_owned),
            timeout,
        ));
        Ok("executed".to_string())
    }
}

#[tokio::test]
async fn code_execution_contract_requires_explicit_rust_runtime_language() {
    let backend = Arc::new(CaptureCodeBackend::default());
    let handler = ExecuteCodeHandler::new(backend.clone());
    let schema = handler.schema();
    let rendered = serde_json::to_string(&schema.parameters).expect("schema json");

    assert_eq!(schema.name, "execute_code");
    assert!(rendered.contains("javascript"));
    assert!(rendered.contains("typescript"));
    assert!(!rendered.contains("\"python\""));

    assert_eq!(
        handler
            .execute(json!({"code": "console.log(1)", "language": "javascript", "timeout": 7}))
            .await
            .expect("execute"),
        "executed"
    );
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [(
            "console.log(1)".to_string(),
            Some("javascript".to_string()),
            Some(7)
        )]
    );
    assert_err_contains(
        handler.execute(json!({"code": "print(1)"})).await,
        "Missing 'language'",
    );
}

#[derive(Default)]
struct CaptureCronBackend {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl CronjobBackend for CaptureCronBackend {
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        toolset: Option<&str>,
        context_from: Option<&Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        workdir: Option<&Value>,
    ) -> Result<String, ToolError> {
        self.calls.lock().expect("calls").push(format!(
            "create:{name}:{schedule}:{task}:{:?}:{:?}:{:?}:{:?}:{:?}",
            toolset, context_from, script, no_agent, workdir
        ));
        Ok("created".to_string())
    }

    async fn list(&self) -> Result<String, ToolError> {
        self.calls.lock().expect("calls").push("list".to_string());
        Ok("[]".to_string())
    }

    async fn update(
        &self,
        id: &str,
        _schedule: Option<&str>,
        _task: Option<&str>,
        enabled: Option<bool>,
        _context_from: Option<&Value>,
        _script: Option<&str>,
        _no_agent: Option<bool>,
        _workdir: Option<&Value>,
    ) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("update:{id}:{enabled:?}"));
        Ok("updated".to_string())
    }

    async fn pause(&self, id: &str) -> Result<String, ToolError> {
        Ok(format!("pause:{id}"))
    }

    async fn resume(&self, id: &str) -> Result<String, ToolError> {
        Ok(format!("resume:{id}"))
    }

    async fn remove(&self, id: &str) -> Result<String, ToolError> {
        Ok(format!("remove:{id}"))
    }

    async fn run(&self, id: &str) -> Result<String, ToolError> {
        Ok(format!("run:{id}"))
    }
}

#[tokio::test]
async fn cronjob_contract_forwards_extended_create_fields_and_rejects_bad_action() {
    let backend = Arc::new(CaptureCronBackend::default());
    let handler = CronjobHandler::new(backend.clone());

    assert_eq!(
        handler
            .execute(json!({
                "action": "create",
                "name": "daily",
                "schedule": "0 9 * * *",
                "task": "summarize",
                "toolset": "web",
                "context_from": ["job_a", "job_b"],
                "script": "echo ok",
                "no_agent": true,
                "workdir": "/tmp"
            }))
            .await
            .expect("create"),
        "created"
    );
    {
        let calls = backend.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("create:daily:0 9 * * *:summarize"));
        assert!(calls[0].contains("Some(\"web\")"));
        assert!(calls[0].contains("Some(true)"));
    }

    assert_err_contains(
        handler.execute(json!({"action": "explode"})).await,
        "Unknown action",
    );
}

#[derive(Default)]
struct CaptureHomeAssistantBackend {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl HomeAssistantBackend for CaptureHomeAssistantBackend {
    async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("entities:{domain:?}"));
        Ok("entities".to_string())
    }

    async fn get_state(&self, entity_id: &str) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("state:{entity_id}"));
        Ok("state".to_string())
    }

    async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("services:{domain:?}"));
        Ok("services".to_string())
    }

    async fn call_service(
        &self,
        service: &str,
        entity_id: &str,
        data: Option<&Value>,
    ) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("call:{service}:{entity_id}:{:?}", data.cloned()));
        Ok("called".to_string())
    }
}

#[tokio::test]
async fn homeassistant_contract_exposes_all_handlers_and_required_params() {
    let backend = Arc::new(CaptureHomeAssistantBackend::default());

    assert_eq!(
        HaListEntitiesHandler::new(backend.clone())
            .execute(json!({"domain": "light"}))
            .await
            .expect("list entities"),
        "entities"
    );
    assert_eq!(
        HaGetStateHandler::new(backend.clone())
            .execute(json!({"entity_id": "light.kitchen"}))
            .await
            .expect("get state"),
        "state"
    );
    assert_eq!(
        HaListServicesHandler::new(backend.clone())
            .execute(json!({"domain": "light"}))
            .await
            .expect("list services"),
        "services"
    );
    assert_eq!(
        HaCallServiceHandler::new(backend.clone())
            .execute(json!({
                "service": "turn_on",
                "entity_id": "light.kitchen",
                "data": {"brightness": 127}
            }))
            .await
            .expect("call service"),
        "called"
    );
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [
            "entities:Some(\"light\")".to_string(),
            "state:light.kitchen".to_string(),
            "services:Some(\"light\")".to_string(),
            "call:turn_on:light.kitchen:Some(Object {\"brightness\": Number(127)})".to_string()
        ]
    );

    assert_err_contains(
        HaGetStateHandler::new(backend.clone())
            .execute(json!({}))
            .await,
        "Missing 'entity_id'",
    );
    assert_err_contains(
        HaCallServiceHandler::new(backend)
            .execute(json!({"service": "turn_on"}))
            .await,
        "Missing 'entity_id'",
    );
}

#[derive(Default)]
struct CaptureMemoryBackend {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl MemoryBackend for CaptureMemoryBackend {
    async fn add(&self, target: &str, content: &str) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("add:{target}:{content}"));
        Ok("added".to_string())
    }

    async fn replace(
        &self,
        target: &str,
        old_text: &str,
        new_content: &str,
    ) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("replace:{target}:{old_text}:{new_content}"));
        Ok("replaced".to_string())
    }

    async fn remove(&self, target: &str, old_text: &str) -> Result<String, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("remove:{target}:{old_text}"));
        Ok("removed".to_string())
    }
}

#[tokio::test]
async fn memory_contract_validates_targets_and_dispatches_actions() {
    let backend = Arc::new(CaptureMemoryBackend::default());
    let handler = MemoryHandler::new(backend.clone());

    assert_eq!(
        handler
            .execute(json!({"action": "add", "target": "memory", "content": "remember this"}))
            .await
            .expect("add"),
        "added"
    );
    assert_eq!(
        handler
            .execute(
                json!({"action": "replace", "target": "user", "old_text": "old", "content": "new"})
            )
            .await
            .expect("replace"),
        "replaced"
    );
    assert_eq!(
        handler
            .execute(json!({"action": "remove", "target": "memory", "old_text": "stale"}))
            .await
            .expect("remove"),
        "removed"
    );
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [
            "add:memory:remember this".to_string(),
            "replace:user:old:new".to_string(),
            "remove:memory:stale".to_string()
        ]
    );
    assert_err_contains(
        handler
            .execute(json!({"action": "add", "target": "other", "content": "bad"}))
            .await,
        "Invalid 'target'",
    );
    assert_err_contains(
        handler
            .execute(json!({"action": "replace", "target": "memory", "content": "new"}))
            .await,
        "Missing 'old_text'",
    );
}

#[derive(Default)]
struct CaptureMessagingBackend {
    calls: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl MessagingBackend for CaptureMessagingBackend {
    async fn send(
        &self,
        platform: &str,
        recipient: &str,
        message: &str,
    ) -> Result<String, ToolError> {
        self.calls.lock().expect("calls").push((
            platform.to_string(),
            recipient.to_string(),
            message.to_string(),
        ));
        Ok("sent".to_string())
    }
}

#[tokio::test]
async fn messaging_contract_accepts_all_gateway_platform_schema_names() {
    let backend = Arc::new(CaptureMessagingBackend::default());
    let handler = SendMessageHandler::new(backend.clone());
    let schema = handler.schema();
    let platforms = schema.parameters.properties.as_ref().expect("properties")["platform"]["enum"]
        .as_array()
        .expect("platform enum");

    for expected in [
        "telegram",
        "discord",
        "slack",
        "whatsapp",
        "signal",
        "email",
        "sms",
        "matrix",
        "mattermost",
        "dingtalk",
        "homeassistant",
        "feishu",
        "ntfy",
        "qqbot",
        "wecom",
        "wecom_callback",
        "webhook",
        "weixin",
        "bluebubbles",
    ] {
        assert!(
            platforms.iter().any(|value| value == expected),
            "missing platform {expected}"
        );
    }

    assert_eq!(
        handler
            .execute(
                json!({"platform": "matrix", "recipient": "!room:example", "message": "hello"})
            )
            .await
            .expect("send"),
        "sent"
    );
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [(
            "matrix".to_string(),
            "!room:example".to_string(),
            "hello".to_string()
        )]
    );
    assert_err_contains(
        handler
            .execute(json!({"recipient": "!room:example", "message": "hello"}))
            .await,
        "Missing 'platform'",
    );
}

#[derive(Default)]
struct CaptureSessionSearchBackend {
    calls: Mutex<Vec<SessionSearchCall>>,
}

type SessionSearchCall = (Option<String>, Option<String>, usize, Option<String>);

#[async_trait]
impl SessionSearchBackend for CaptureSessionSearchBackend {
    async fn search(
        &self,
        query: Option<&str>,
        role_filter: Option<&str>,
        limit: usize,
        current_session_id: Option<&str>,
    ) -> Result<String, ToolError> {
        self.calls.lock().expect("calls").push((
            query.map(ToOwned::to_owned),
            role_filter.map(ToOwned::to_owned),
            limit,
            current_session_id.map(ToOwned::to_owned),
        ));
        Ok("results".to_string())
    }
}

#[tokio::test]
async fn session_search_contract_caps_limit_and_forwards_filters() {
    let backend = Arc::new(CaptureSessionSearchBackend::default());
    let handler = SessionSearchHandler::new(backend.clone());

    assert_eq!(
        handler
            .execute(json!({
                "query": "approval",
                "role_filter": "user,assistant",
                "limit": 99,
                "current_session_id": "current"
            }))
            .await
            .expect("search"),
        "results"
    );
    assert_eq!(
        backend.calls.lock().expect("calls").as_slice(),
        [(
            Some("approval".to_string()),
            Some("user,assistant".to_string()),
            5,
            Some("current".to_string())
        )]
    );
}

#[tokio::test]
async fn process_registry_contract_round_trips_entries_and_normalizes_status() {
    let handler = ProcessRegistryHandler::default();

    let registered: Value = serde_json::from_str(
        &handler
            .execute(json!({
                "action": "register",
                "name": "worker",
                "pid": 42,
                "command": "sleep 1",
                "status": "unknown"
            }))
            .await
            .expect("register"),
    )
    .expect("json");
    assert_eq!(registered["status"], "registered");

    let got: Value = serde_json::from_str(
        &handler
            .execute(json!({"action": "get", "name": "worker"}))
            .await
            .expect("get"),
    )
    .expect("json");
    assert_eq!(got["entry"]["status"], "running");
    assert_eq!(got["entry"]["command"], "sleep 1");

    let updated: Value = serde_json::from_str(
        &handler
            .execute(json!({"action": "update", "name": "worker", "status": "failed"}))
            .await
            .expect("update"),
    )
    .expect("json");
    assert_eq!(updated["entry"]["status"], "failed");

    let cleared: Value = serde_json::from_str(
        &handler
            .execute(json!({"action": "clear"}))
            .await
            .expect("clear"),
    )
    .expect("json");
    assert_eq!(cleared["removed"], 1);

    assert_err_contains(
        handler
            .execute(json!({"action": "register", "name": "bad", "pid": 0}))
            .await,
        "positive pid",
    );
}

struct EchoHandler;

#[async_trait]
impl ToolHandler for EchoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        if params.get("fail").and_then(Value::as_bool).unwrap_or(false) {
            return Err(ToolError::ExecutionFailed("boom".to_string()));
        }
        Ok(params.to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema("echo", "Echo back input", JsonSchema::new("object"))
    }
}

#[tokio::test]
async fn registry_contract_lists_dispatches_and_wraps_errors_as_json() {
    let registry = ToolRegistry::with_max_result_size(80);
    let handler = Arc::new(EchoHandler);
    registry.register(
        "echo",
        "test",
        handler.schema(),
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Echo tool",
        "*",
        None,
    );

    assert!(registry.is_available("echo"));
    assert_eq!(registry.get_definitions()[0].name, "echo");
    assert_eq!(registry.get_tool("echo").expect("tool").toolset, "test");

    let ok: Value = serde_json::from_str(
        &registry
            .dispatch_async("echo", json!({"message": "ok"}))
            .await,
    )
    .expect("json");
    assert_eq!(ok["message"], "ok");

    let error: Value =
        serde_json::from_str(&registry.dispatch_async("echo", json!({"fail": true})).await)
            .expect("json");
    assert!(error["error"].as_str().unwrap_or_default().contains("boom"));

    let missing: Value =
        serde_json::from_str(&registry.dispatch_async("missing", json!({})).await).expect("json");
    assert!(missing["error"]
        .as_str()
        .unwrap_or_default()
        .contains("Tool not found"));
}

struct MinimalTerminalBackend;

#[async_trait]
impl TerminalBackend for MinimalTerminalBackend {
    async fn execute_command(
        &self,
        _command: &str,
        _timeout: Option<u64>,
        _workdir: Option<&str>,
        _background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        Ok(CommandOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        Ok(format!("{path}:{offset:?}:{limit:?}"))
    }

    async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
        Ok(())
    }

    async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
        Ok(true)
    }
}

#[tokio::test]
async fn terminal_backend_contract_rejects_stdin_by_default() {
    let backend = MinimalTerminalBackend;
    let err = backend
        .execute_command_with_stdin("cat", None, None, false, false, Some("input"))
        .await
        .expect_err("stdin unsupported by default");
    assert!(err.to_string().contains("stdin_data is not supported"));
}
