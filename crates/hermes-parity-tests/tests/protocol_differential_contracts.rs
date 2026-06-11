use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_acp::protocol::{AcpMethod, AcpResponse};
use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
use hermes_gateway::commands::{canonical_command_name, is_registered_command_name};
use hermes_mcp::McpServer;
use hermes_tools::ToolRegistry;
use serde::Deserialize;
use serde_json::{json, Value};

struct EchoProtocolArgs;

#[async_trait]
impl ToolHandler for EchoProtocolArgs {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        Ok(params.to_string())
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "sota_protocol_echo",
            "Echo protocol arguments for differential contract tests",
            JsonSchema::new("object"),
        )
    }
}

#[derive(Debug, Deserialize)]
struct ProtocolSuite {
    schema_version: u32,
    suite: String,
    cases: Vec<ProtocolCase>,
}

#[derive(Debug, Deserialize)]
struct ProtocolCase {
    id: String,
    kind: String,
    input: Value,
    expected: Value,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_suite() -> ProtocolSuite {
    let path = repo_root()
        .join("crates/hermes-parity-tests/tests/fixtures/protocol_differential_contracts.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed reading {}: {}", path.display(), err));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed parsing {}: {}", path.display(), err))
}

fn protocol_mcp_server() -> McpServer {
    let registry = ToolRegistry::new();
    let handler = Arc::new(EchoProtocolArgs);
    registry.register(
        "sota_protocol_echo",
        "test",
        handler.schema(),
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Echo protocol arguments",
        "test",
        None,
    );
    McpServer::new(Arc::new(registry))
}

fn acp_method_label(method: &str) -> String {
    match AcpMethod::from(method) {
        AcpMethod::Initialize => "Initialize",
        AcpMethod::Authenticate => "Authenticate",
        AcpMethod::NewSession => "NewSession",
        AcpMethod::LoadSession => "LoadSession",
        AcpMethod::ResumeSession => "ResumeSession",
        AcpMethod::ForkSession => "ForkSession",
        AcpMethod::ListSessions => "ListSessions",
        AcpMethod::Cancel => "Cancel",
        AcpMethod::Prompt => "Prompt",
        AcpMethod::SetSessionModel => "SetSessionModel",
        AcpMethod::SetSessionMode => "SetSessionMode",
        AcpMethod::SetConfigOption => "SetConfigOption",
        AcpMethod::CreateConversation => "CreateConversation",
        AcpMethod::SendMessage => "SendMessage",
        AcpMethod::GetHistory => "GetHistory",
        AcpMethod::ListTools => "ListTools",
        AcpMethod::ExecuteTool => "ExecuteTool",
        AcpMethod::GetStatus => "GetStatus",
        AcpMethod::Unknown(_) => "Unknown",
    }
    .to_string()
}

fn assert_expected_subset(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Object(actual_obj), Value::Object(expected_obj)) => {
            for (key, expected_value) in expected_obj {
                let actual_value = actual_obj
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}.{key} missing in actual {actual}"));
                assert_expected_subset(actual_value, expected_value, &format!("{path}.{key}"));
            }
        }
        _ => assert_eq!(actual, expected, "mismatch at {path}"),
    }
}

#[test]
fn protocol_differential_fixture_is_complete_and_unique() {
    let suite = load_suite();
    assert_eq!(suite.schema_version, 1);
    assert_eq!(suite.suite, "protocol_differential_contracts");
    assert!(
        suite.cases.len() >= 7,
        "expected ACP, MCP, and gateway protocol cases"
    );

    let mut ids = std::collections::BTreeSet::new();
    let mut kinds = std::collections::BTreeSet::new();
    for case in &suite.cases {
        assert!(
            ids.insert(case.id.as_str()),
            "duplicate case id {}",
            case.id
        );
        kinds.insert(case.kind.as_str());
        assert!(
            case.input.is_object(),
            "case {} input must be object",
            case.id
        );
        assert!(
            case.expected.is_object(),
            "case {} expected must be object",
            case.id
        );
    }

    for required in [
        "acp_method_alias",
        "acp_success_response",
        "mcp_initialize",
        "mcp_argument_coercion",
        "gateway_command_canonicalization",
    ] {
        assert!(
            kinds.contains(required),
            "missing protocol case kind {required}"
        );
    }
}

#[tokio::test]
async fn protocol_differential_fixture_matches_runtime_serialization() {
    let suite = load_suite();
    let mcp_server = protocol_mcp_server();

    for case in suite.cases {
        match case.kind.as_str() {
            "acp_method_alias" => {
                let method = case.input["method"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{} missing input.method", case.id));
                let actual = json!({"canonical": acp_method_label(method)});
                assert_eq!(actual, case.expected, "case {}", case.id);
            }
            "acp_success_response" => {
                let response = AcpResponse::success(
                    Some(case.input["id"].clone()),
                    case.input["result"].clone(),
                );
                let actual = serde_json::to_value(response).expect("serialize ACP response");
                assert_eq!(actual, case.expected, "case {}", case.id);
            }
            "mcp_initialize" => {
                let actual = mcp_server
                    .handle_request("initialize", Value::Object(Default::default()))
                    .await
                    .unwrap_or_else(|err| panic!("{} initialize failed: {}", case.id, err));
                assert_expected_subset(&actual, &case.expected, &case.id);
            }
            "mcp_argument_coercion" => {
                let arguments = case.input["arguments"].clone();
                let result = mcp_server
                    .handle_request(
                        "tools/call",
                        json!({
                            "name": "sota_protocol_echo",
                            "arguments": arguments
                        }),
                    )
                    .await
                    .unwrap_or_else(|err| panic!("{} tools/call failed: {}", case.id, err));
                let text = result["content"][0]["text"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{} missing MCP text result: {}", case.id, result));
                let actual: Value = serde_json::from_str(text)
                    .unwrap_or_else(|err| panic!("{} invalid echo JSON: {}", case.id, err));
                assert_eq!(actual, case.expected, "case {}", case.id);
            }
            "gateway_command_canonicalization" => {
                let command = case.input["command"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{} missing input.command", case.id));
                let actual = json!({
                    "canonical": canonical_command_name(command),
                    "registered": is_registered_command_name(command),
                });
                assert_eq!(actual, case.expected, "case {}", case.id);
            }
            other => panic!("unknown protocol case kind {other} in {}", case.id),
        }
    }
}
