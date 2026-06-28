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

include!("tests/lifecycle_auth.rs");

include!("tests/session_state.rs");

include!("tests/prompt_resources.rs");

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

include!("tests/queue_and_steer.rs");

include!("tests/tools_and_execution.rs");
