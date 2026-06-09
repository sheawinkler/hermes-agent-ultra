//! PromptExecutor trait -- delegates agent execution to the integrator.
//!
//! The crate does not depend on hermes-agent directly.
//! hermes-cli provides a concrete implementation that bridges to the agent loop.

use crate::session::PipeSession;
use serde::Serialize;
use serde_json::Value;

use hermes_acp::protocol::{StopReason, Usage};

// ---------------------------------------------------------------------------
// StreamContent -- serialized ContentBlock aligned with Cherry ACP SDK Zod schema
// ---------------------------------------------------------------------------

/// Content block for streaming events.
/// MUST include type field (Cherry ACP SDK silently discards without it).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamContent {
    #[serde(rename = "text")]
    Text { text: String },
}

// ---------------------------------------------------------------------------
// StreamEvent -- the streaming event pushed during prompt execution
// ---------------------------------------------------------------------------

/// A streaming event pushed via mpsc during prompt execution.
///
/// Serialized with #[serde(tag = "sessionUpdate")] to match Cherry's expected format:
/// `json
/// { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "..." } }
/// `
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum StreamEvent {
    AgentMessageChunk {
        content: StreamContent,
    },
    AgentThoughtChunk {
        content: StreamContent,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_input: Option<Value>,
        status: String, // "pending" | "completed"
    },
    ToolCallUpdate {
        tool_call_id: String,
        status: String, // "pending" | "completed"
        #[serde(skip_serializing_if = "Vec::is_empty")]
        content: Vec<StreamContent>,
    },
}

// ---------------------------------------------------------------------------
// PromptResult
// ---------------------------------------------------------------------------

/// Final result of a prompt execution.
pub struct PromptResult {
    pub stop_reason: StopReason,
    pub usage: Option<Usage>,
    /// The complete assistant message produced during execution.
    /// Used to update session history for multi-turn conversations.
    pub assistant_message: Option<String>,
}

// ---------------------------------------------------------------------------
// PromptExecutor trait
// ---------------------------------------------------------------------------

/// Delegates prompt execution to the integrator (e.g. hermes-cli).
///
/// During execution, the implementor pushes StreamEvents through event_tx
/// for real-time streaming to the ACP client.
#[async_trait::async_trait]
pub trait PromptExecutor: Send + Sync {
    async fn execute(
        &self,
        session: &PipeSession,
        prompt_text: &str,
        history: &[Value],
        event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<PromptResult, String>;
}

/// Concrete executor implementations.
pub mod llm;
