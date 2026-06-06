//! Complete ACP protocol types.
//!
//! Defines the full set of ACP JSON-RPC methods, request/response types,
//! capability declarations, content blocks, and session update structures.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC envelope
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// ACP methods — full protocol surface
// ---------------------------------------------------------------------------

/// Supported ACP methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpMethod {
    // -- Lifecycle --
    Initialize,
    Authenticate,

    // -- Session management --
    NewSession,
    LoadSession,
    ResumeSession,
    ForkSession,
    ListSessions,
    Cancel,

    // -- Prompt (core) --
    Prompt,

    // -- Session configuration --
    SetSessionModel,
    SetSessionMode,
    SetConfigOption,

    // -- Legacy / compatibility --
    CreateConversation,
    SendMessage,
    GetHistory,
    ListTools,
    ExecuteTool,
    GetStatus,

    /// Unknown method.
    Unknown(String),
}

impl From<&str> for AcpMethod {
    fn from(s: &str) -> Self {
        match s {
            // ACP v0.9+ protocol methods
            "initialize" => Self::Initialize,
            "authenticate" => Self::Authenticate,
            "session/new" | "new_session" => Self::NewSession,
            "session/load" | "load_session" => Self::LoadSession,
            "session/resume" | "resume_session" => Self::ResumeSession,
            "session/fork" | "fork_session" => Self::ForkSession,
            "session/list" | "list_sessions" => Self::ListSessions,
            "session/cancel" | "cancel" => Self::Cancel,
            "prompt" => Self::Prompt,
            "session/set_model" | "set_session_model" => Self::SetSessionModel,
            "session/set_mode" | "set_session_mode" => Self::SetSessionMode,
            "session/set_config" | "session/set_config_option" | "set_config_option" => {
                Self::SetConfigOption
            }

            // Legacy methods
            "conversation.create" => Self::CreateConversation,
            "message.send" => Self::SendMessage,
            "history.get" => Self::GetHistory,
            "tools.list" => Self::ListTools,
            "tools.execute" => Self::ExecuteTool,
            "status.get" => Self::GetStatus,

            other => Self::Unknown(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Capability declarations
// ---------------------------------------------------------------------------

/// Agent capabilities advertised during `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentCapabilities {
    #[serde(rename = "loadSession", alias = "load_session", default)]
    pub load_session: bool,
    #[serde(
        rename = "promptCapabilities",
        alias = "prompt_capabilities",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt_capabilities: Option<PromptCapabilities>,
    #[serde(default)]
    #[serde(
        rename = "sessionCapabilities",
        alias = "session_capabilities",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_capabilities: Option<SessionCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

/// Prompt-level capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
}

/// Session-level capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionCapabilities {
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub list: bool,
    #[serde(default)]
    pub resume: bool,
}

/// Client capabilities received during `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
}

/// Implementation identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

/// Authentication method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub method_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// Initialize response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResponse {
    #[serde(rename = "protocolVersion", alias = "protocol_version")]
    pub protocol_version: u32,
    #[serde(rename = "agentInfo", alias = "agent_info")]
    pub agent_info: Implementation,
    #[serde(rename = "agentCapabilities", alias = "agent_capabilities")]
    pub agent_capabilities: AgentCapabilities,
    #[serde(
        rename = "authMethods",
        alias = "auth_methods",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auth_methods: Option<Vec<AuthMethod>>,
}

// ---------------------------------------------------------------------------
// Content blocks
// ---------------------------------------------------------------------------

/// Content block types for prompts and responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        #[serde(default)]
        url: String,
        #[serde(default)]
        data: Option<String>,
        #[serde(rename = "mimeType", alias = "mime_type", default)]
        mime_type: Option<String>,
        #[serde(default)]
        alt: Option<String>,
    },
    Audio {
        url: String,
    },
    Resource {
        uri: String,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Extract text content from a content block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// Extract plain text from a list of content blocks.
pub fn extract_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| b.as_text())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Session update types (server → client notifications)
// ---------------------------------------------------------------------------

/// Session update notification sent from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    /// Streaming agent message text.
    AgentMessageText { text: String },
    /// Streaming agent thinking/reasoning text.
    AgentThoughtText { text: String },
    /// Tool call started.
    ToolCallStart {
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<Value>,
    },
    /// Tool call completed.
    ToolCallComplete {
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// Available slash commands updated.
    AvailableCommandsUpdate { commands: Vec<AvailableCommand> },
}

/// A slash command available in the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
    #[serde(
        rename = "inputHint",
        alias = "input_hint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_hint: Option<String>,
}

/// Entry in a native ACP plan update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub priority: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Usage stats
// ---------------------------------------------------------------------------

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(rename = "inputTokens", alias = "input_tokens", default)]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", alias = "output_tokens", default)]
    pub output_tokens: u64,
    #[serde(rename = "totalTokens", alias = "total_tokens", default)]
    pub total_tokens: u64,
    #[serde(
        rename = "thoughtTokens",
        alias = "thought_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thought_tokens: Option<u64>,
    #[serde(
        rename = "cachedReadTokens",
        alias = "cached_read_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_read_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Prompt response
// ---------------------------------------------------------------------------

/// Stop reason for a prompt response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    Cancelled,
    MaxTokens,
    Refusal,
    Error,
}

/// Response to a `prompt` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResponse {
    #[serde(rename = "stopReason", alias = "stop_reason")]
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

// ---------------------------------------------------------------------------
// MCP server config blocks
// ---------------------------------------------------------------------------

/// MCP server configuration that can be provided by the ACP client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerConfig {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: Vec<EnvVar>,
    },
    Http {
        name: String,
        url: String,
        #[serde(default)]
        headers: Vec<EnvVar>,
    },
    Sse {
        name: String,
        url: String,
        #[serde(default)]
        headers: Vec<EnvVar>,
    },
}

/// An environment variable / header key-value pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Session info (for list_sessions response)
// ---------------------------------------------------------------------------

/// Session info for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSessionInfo {
    #[serde(rename = "sessionId", alias = "session_id")]
    pub session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acp_method_from_str() {
        assert_eq!(AcpMethod::from("initialize"), AcpMethod::Initialize);
        assert_eq!(AcpMethod::from("session/new"), AcpMethod::NewSession);
        assert_eq!(AcpMethod::from("new_session"), AcpMethod::NewSession);
        assert_eq!(AcpMethod::from("prompt"), AcpMethod::Prompt);
        assert_eq!(AcpMethod::from("cancel"), AcpMethod::Cancel);
        assert_eq!(
            AcpMethod::from("session/set_config_option"),
            AcpMethod::SetConfigOption
        );
        assert_eq!(
            AcpMethod::from("conversation.create"),
            AcpMethod::CreateConversation
        );
        match AcpMethod::from("unknown.method") {
            AcpMethod::Unknown(s) => assert_eq!(s, "unknown.method"),
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_acp_response_success() {
        let resp =
            AcpResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
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
    fn test_content_block_extract_text() {
        let blocks = vec![
            ContentBlock::text("hello"),
            ContentBlock::Image {
                url: "http://img.png".into(),
                data: None,
                mime_type: None,
                alt: None,
            },
            ContentBlock::text("world"),
        ];
        assert_eq!(extract_text(&blocks), "hello\nworld");

        let image: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "data": "aGVsbG8=",
            "mimeType": "image/png"
        }))
        .unwrap();
        match image {
            ContentBlock::Image {
                url,
                data,
                mime_type,
                ..
            } => {
                assert_eq!(url, "");
                assert_eq!(data.as_deref(), Some("aGVsbG8="));
                assert_eq!(mime_type.as_deref(), Some("image/png"));
            }
            _ => panic!("expected image block"),
        }
    }

    #[test]
    fn test_acp_request_serde() {
        let req = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "prompt".to_string(),
            params: Some(serde_json::json!({"text": "hello"})),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AcpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "prompt");
    }

    #[test]
    fn test_capabilities_serde() {
        let caps = AgentCapabilities {
            load_session: true,
            prompt_capabilities: Some(PromptCapabilities { image: true }),
            session_capabilities: Some(SessionCapabilities {
                fork: true,
                list: true,
                resume: true,
            }),
            streaming: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["loadSession"], true);
        assert_eq!(json["promptCapabilities"]["image"], true);
        assert_eq!(json["sessionCapabilities"]["fork"], true);
        assert_eq!(json["streaming"], true);
    }
}
