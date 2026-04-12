//! ACP request handler.

use crate::protocol::{AcpMethod, AcpRequest, AcpResponse};

/// Trait for handling ACP requests.
#[async_trait::async_trait]
pub trait AcpHandler: Send + Sync {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse;
}

/// Default ACP handler that dispatches methods.
pub struct DefaultAcpHandler;

#[async_trait::async_trait]
impl AcpHandler for DefaultAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        let method = AcpMethod::from(request.method.as_str());
        match method {
            AcpMethod::CreateConversation => {
                AcpResponse::success(
                    request.id,
                    serde_json::json!({
                        "conversation_id": uuid::Uuid::new_v4().to_string(),
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
            AcpMethod::ListTools => {
                AcpResponse::success(
                    request.id,
                    serde_json::json!({ "tools": [] }),
                )
            }
            AcpMethod::Cancel => {
                AcpResponse::success(request.id, serde_json::json!({"cancelled": true}))
            }
            AcpMethod::Unknown(method) => {
                AcpResponse::error(request.id, -32601, format!("Method not found: {}", method))
            }
            _ => {
                AcpResponse::error(request.id, -32000, "Method not yet implemented")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_conversation() {
        let handler = DefaultAcpHandler;
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
        let handler = DefaultAcpHandler;
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
}
