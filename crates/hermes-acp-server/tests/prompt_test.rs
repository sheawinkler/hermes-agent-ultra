//! Integration tests for prompt execution and streaming events.
//!
//! Verifies: mock executor produces correct StreamEvents, event bridge
//! formats them as valid NDJSON session/update notifications, and the
//! full prompt lifecycle (prompt -> streaming chunks -> stopReason) works.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use hermes_acp::protocol::StopReason;
use hermes_acp_server::{
    AcpPipeServer, AcpServerConfig, AgentInfo, PipeSession, PromptExecutor, PromptResult,
    StreamContent, StreamEvent,
};

// ---------------------------------------------------------------------------
// Mock executor
// ---------------------------------------------------------------------------

struct MockExecutor {
    chunks: Vec<String>,
}

#[async_trait]
impl PromptExecutor for MockExecutor {
    async fn execute(
        &self,
        _session: &PipeSession,
        _prompt_text: &str,
        _history: &[Value],
        event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<PromptResult, String> {
        for chunk in &self.chunks {
            let _ = event_tx
                .send(StreamEvent::AgentMessageChunk {
                    content: StreamContent::Text {
                        text: chunk.clone(),
                    },
                })
                .await;
        }
        let assistant_message = self.chunks.join("");
        Ok(PromptResult {
            stop_reason: StopReason::EndTurn,
            usage: None,
            assistant_message: if assistant_message.is_empty() {
                None
            } else {
                Some(assistant_message)
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prompt_streams_chunks_then_stop_reason() {
    let pipe = common::test_pipe("stream-chunks");
    let executor = Arc::new(MockExecutor {
        chunks: vec!["Hello".to_string(), " world".to_string(), "!".to_string()],
    });
    let config = AcpServerConfig {
        pipe_path: pipe.clone(),
        max_connections: 2,
        prompt_timeout_secs: 300,
        agent_info: AgentInfo {
            name: "test-agent".to_string(),
            title: "Test".to_string(),
            version: "0.0.1".to_string(),
        },
        executor,
        event_sink: None,
    };

    let server = AcpPipeServer::new(config).unwrap();
    let server_arc = Arc::new(server);
    let srv = server_arc.clone();
    let handle = tokio::spawn(async move { srv.run().await.unwrap() });

    let mut client = common::connect_client(&pipe).await;

    // initialize
    let resp = common::roundtrip(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": "test", "title": "T", "version": "1" } }
        }),
    )
    .await;
    assert_eq!(resp["id"], 1);
    assert!(resp.get("result").is_some());

    // session/new
    let resp = common::roundtrip(
        &mut *client,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {} }),
    )
    .await;
    assert_eq!(resp["id"], 2);
    assert!(resp["result"]["sessionId"].is_string());

    // session/prompt -- read all responses
    common::send_request(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
            "params": { "prompt": [{ "type": "text", "text": "say hello" }] }
        }),
    )
    .await;

    let messages = common::read_all_ndjson(&mut *client, std::time::Duration::from_secs(3)).await;

    // Separate the final response (has "id") from notifications (has "method")
    let final_resp = messages
        .iter()
        .find(|m| m.get("id").and_then(|v| v.as_u64()) == Some(3))
        .expect("should have response with id=3")
        .clone();
    let notifications: Vec<&Value> = messages
        .iter()
        .filter(|m| {
            m.get("method")
                .map(|v| v.as_str() == Some("session/update"))
                .unwrap_or(false)
        })
        .collect();

    // Verify final response
    assert_eq!(final_resp["id"], 3);
    assert_eq!(final_resp["result"]["stopReason"], "end_turn");

    // Verify streaming notifications
    assert!(
        !notifications.is_empty(),
        "expected streaming notifications, got {} messages total",
        messages.len()
    );

    for notif in &notifications {
        assert_eq!((*notif)["jsonrpc"], "2.0");
        assert_eq!((*notif)["method"], "session/update");
        assert!((*notif)["params"]["sessionId"].is_string());

        let update = &(*notif)["params"]["update"];
        let update_type = update["sessionUpdate"].as_str().unwrap();
        assert!(update_type == "agent_message_chunk" || update_type == "agent_thought_chunk");
        assert_eq!(update["content"]["type"], "text");
        assert!(update["content"]["text"].is_string());
    }

    // Verify chunk content
    let texts: Vec<&str> = notifications
        .iter()
        .filter_map(|n| {
            let u = &(*n)["params"]["update"];
            if u["sessionUpdate"] == "agent_message_chunk" {
                u["content"]["text"].as_str()
            } else {
                None
            }
        })
        .collect();

    assert_eq!(texts.len(), 3);
    assert_eq!(texts.join(""), "Hello world!");

    server_arc.shutdown();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;
}

#[tokio::test]
async fn prompt_without_session_returns_error() {
    let pipe = common::test_pipe("no-session");
    let config = AcpServerConfig {
        pipe_path: pipe.clone(),
        max_connections: 2,
        prompt_timeout_secs: 300,
        agent_info: AgentInfo {
            name: "test-agent".to_string(),
            title: "Test".to_string(),
            version: "0.0.1".to_string(),
        },
        executor: Arc::new(MockExecutor { chunks: vec![] }),
        event_sink: None,
    };

    let server = AcpPipeServer::new(config).unwrap();
    let server_arc = Arc::new(server);
    let srv = server_arc.clone();
    let handle = tokio::spawn(async move { srv.run().await.unwrap() });

    let mut client = common::connect_client(&pipe).await;

    // initialize only (skip session/new)
    let _ = common::roundtrip(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": "test", "title": "T", "version": "1" } }
        }),
    )
    .await;

    // prompt without session -> error
    let resp = common::roundtrip(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
            "params": { "prompt": [{ "type": "text", "text": "hello" }] }
        }),
    )
    .await;

    assert!(
        resp.get("error").is_some(),
        "expected error for prompt without session"
    );

    server_arc.shutdown();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;
}

#[tokio::test]
async fn empty_prompt_returns_error() {
    let pipe = common::test_pipe("empty-prompt");
    let config = AcpServerConfig {
        pipe_path: pipe.clone(),
        max_connections: 2,
        prompt_timeout_secs: 300,
        agent_info: AgentInfo {
            name: "test-agent".to_string(),
            title: "Test".to_string(),
            version: "0.0.1".to_string(),
        },
        executor: Arc::new(MockExecutor { chunks: vec![] }),
        event_sink: None,
    };

    let server = AcpPipeServer::new(config).unwrap();
    let server_arc = Arc::new(server);
    let srv = server_arc.clone();
    let handle = tokio::spawn(async move { srv.run().await.unwrap() });

    let mut client = common::connect_client(&pipe).await;

    // initialize + session/new
    let _ = common::roundtrip(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": "test", "title": "T", "version": "1" } }
        }),
    )
    .await;
    let _ = common::roundtrip(
        &mut *client,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {} }),
    )
    .await;

    // Empty prompt
    let resp = common::roundtrip(
        &mut *client,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
            "params": { "prompt": [] }
        }),
    )
    .await;

    assert!(resp.get("error").is_some());
    assert_eq!(resp["error"]["code"], -32600);

    server_arc.shutdown();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;
}
