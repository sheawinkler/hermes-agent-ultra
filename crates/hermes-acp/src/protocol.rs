//! ACP protocol types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// ACP JSON-RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// ACP JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AcpError>,
}

/// ACP error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Supported ACP methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpMethod {
    /// Start a new conversation.
    CreateConversation,
    /// Send a message to the agent.
    SendMessage,
    /// Get conversation history.
    GetHistory,
    /// List available tools.
    ListTools,
    /// Execute a tool directly.
    ExecuteTool,
    /// Get agent status.
    GetStatus,
    /// Cancel current operation.
    Cancel,
    /// Unknown method.
    Unknown(String),
}

impl From<&str> for AcpMethod {
    fn from(s: &str) -> Self {
        match s {
            "conversation.create" => Self::CreateConversation,
            "message.send" => Self::SendMessage,
            "history.get" => Self::GetHistory,
            "tools.list" => Self::ListTools,
            "tools.execute" => Self::ExecuteTool,
            "status.get" => Self::GetStatus,
            "cancel" => Self::Cancel,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl AcpResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(AcpError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acp_method_from_str() {
        assert_eq!(AcpMethod::from("conversation.create"), AcpMethod::CreateConversation);
        assert_eq!(AcpMethod::from("message.send"), AcpMethod::SendMessage);
        match AcpMethod::from("unknown.method") {
            AcpMethod::Unknown(s) => assert_eq!(s, "unknown.method"),
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_acp_response_success() {
        let resp = AcpResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_acp_response_error() {
        let resp = AcpResponse::error(Some(serde_json::json!(1)), -32600, "Invalid request");
        assert!(resp.result.is_none());
        assert_eq!(resp.error.as_ref().unwrap().code, -32600);
    }

    #[test]
    fn test_acp_request_serde() {
        let req = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "message.send".to_string(),
            params: Some(serde_json::json!({"text": "hello"})),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AcpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "message.send");
    }
}
