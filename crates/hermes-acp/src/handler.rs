//! ACP request handler.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::protocol::{AcpMethod, AcpRequest, AcpResponse};

/// Trait for handling ACP requests.
#[async_trait::async_trait]
pub trait AcpHandler: Send + Sync {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse;
}

#[derive(Default)]
struct AcpState {
    /// `conversation_id` → ordered JSON messages (role/content).
    conversations: HashMap<String, Vec<Value>>,
}

/// Default ACP handler with in-memory conversations, messages, and tool echo.
pub struct DefaultAcpHandler {
    state: Arc<Mutex<AcpState>>,
}

impl Default for DefaultAcpHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultAcpHandler {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AcpState::default())),
        }
    }
}

fn params_obj(params: &Option<Value>) -> Option<&serde_json::Map<String, Value>> {
    params.as_ref()?.as_object()
}

#[async_trait::async_trait]
impl AcpHandler for DefaultAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        let method = AcpMethod::from(request.method.as_str());
        match method {
            AcpMethod::CreateConversation => {
                let id = uuid::Uuid::new_v4().to_string();
                {
                    let mut g = self.state.lock().await;
                    g.conversations.insert(id.clone(), Vec::new());
                }
                AcpResponse::success(
                    request.id,
                    serde_json::json!({
                        "conversation_id": id,
                    }),
                )
            }
            AcpMethod::SendMessage => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "message.send: missing params object",
                    );
                };
                let Some(conv_id) = p.get("conversation_id").and_then(|v| v.as_str()) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "message.send: missing conversation_id",
                    );
                };
                let text = p
                    .get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| p.get("content").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let msg_id = uuid::Uuid::new_v4().to_string();
                let entry = json!({
                    "id": msg_id,
                    "role": "user",
                    "content": text,
                });
                {
                    let mut g = self.state.lock().await;
                    let Some(msgs) = g.conversations.get_mut(conv_id) else {
                        return AcpResponse::error(
                            request.id,
                            -32602,
                            format!("message.send: unknown conversation_id '{}'", conv_id),
                        );
                    };
                    msgs.push(entry);
                }
                AcpResponse::success(
                    request.id,
                    json!({ "message_id": msg_id, "conversation_id": conv_id }),
                )
            }
            AcpMethod::GetHistory => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "history.get: missing params object",
                    );
                };
                let Some(conv_id) = p.get("conversation_id").and_then(|v| v.as_str()) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "history.get: missing conversation_id",
                    );
                };
                let messages = {
                    let g = self.state.lock().await;
                    g.conversations
                        .get(conv_id)
                        .cloned()
                        .unwrap_or_default()
                };
                AcpResponse::success(request.id, json!({ "messages": messages }))
            }
            AcpMethod::ListTools => {
                AcpResponse::success(
                    request.id,
                    serde_json::json!({ "tools": [] }),
                )
            }
            AcpMethod::ExecuteTool => {
                let Some(p) = params_obj(&request.params) else {
                    return AcpResponse::error(
                        request.id,
                        -32602,
                        "tools.execute: missing params object",
                    );
                };
                let name = p
                    .get("name")
                    .or_else(|| p.get("tool"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let arguments = p.get("arguments").cloned().unwrap_or(Value::Null);
                AcpResponse::success(
                    request.id,
                    json!({
                        "tool": name,
                        "arguments": arguments,
                        "result": format!("ACP default handler echo for tool '{}'", name),
                        "note": "Wire a custom AcpHandler to run real tools.",
                    }),
                )
            }
            AcpMethod::GetStatus => {
                AcpResponse::success(
                    request.id,
                    serde_json::json!({
                        "status": "ready",
                        "version": env!("CARGO_PKG_VERSION"),
                    }),
                )
            }
            AcpMethod::Cancel => {
                AcpResponse::success(request.id, serde_json::json!({"cancelled": true}))
            }
            AcpMethod::Unknown(method) => {
                AcpResponse::error(request.id, -32601, format!("Method not found: {}", method))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_create_conversation() {
        let handler = DefaultAcpHandler::default();
        let req = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "conversation.create".to_string(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        assert!(result.get("conversation_id").is_some());
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let handler = DefaultAcpHandler::default();
        let req = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "foo.bar".to_string(),
            params: None,
        };
        let resp = handler.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_send_and_history_roundtrip() {
        let handler = DefaultAcpHandler::new();
        let create = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "conversation.create".to_string(),
            params: None,
        };
        let cid = handler
            .handle_request(create)
            .await
            .result
            .unwrap()["conversation_id"]
            .as_str()
            .unwrap()
            .to_string();

        let send = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "message.send".to_string(),
            params: Some(json!({
                "conversation_id": cid,
                "text": "hello acp",
            })),
        };
        assert!(handler.handle_request(send).await.error.is_none());

        let hist = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "history.get".to_string(),
            params: Some(json!({ "conversation_id": cid })),
        };
        let msgs = handler.handle_request(hist).await.result.unwrap();
        assert_eq!(msgs["messages"].as_array().unwrap().len(), 1);
    }
}
