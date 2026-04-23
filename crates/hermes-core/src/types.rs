use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MessageRole
// ---------------------------------------------------------------------------

/// Role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

// ---------------------------------------------------------------------------
// CacheControl
// ---------------------------------------------------------------------------

/// Cache hint type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    Ephemeral,
    Persistent,
}

/// Cache control annotation for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    pub cache_type: CacheType,
}

// ---------------------------------------------------------------------------
// FunctionCall / ToolCall
// ---------------------------------------------------------------------------

/// A function call within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A tool call emitted by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(flatten)]
    pub function: FunctionCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// ReasoningContent
// ---------------------------------------------------------------------------

/// Format of the reasoning content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningFormat {
    Simple,
    Details,
}

/// Reasoning content parsed from the LLM response.
///
/// Supports multiple formats used by different providers:
/// - `reasoning_content` (simple string)
/// - `reasoning` (simple string)
/// - `reasoning_details` (structured array)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningContent {
    pub text: String,
    pub format: ReasoningFormat,
}

impl ReasoningContent {
    /// Parse reasoning content from a raw JSON value.
    ///
    /// Accepts:
    /// - A plain string (from `reasoning_content` or `reasoning`)
    /// - An object with a `text` field
    /// - An array of objects with `text` fields (from `reasoning_details`)
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(s) => Some(ReasoningContent {
                text: s.clone(),
                format: ReasoningFormat::Simple,
            }),
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                    Some(ReasoningContent {
                        text: text.to_string(),
                        format: ReasoningFormat::Details,
                    })
                } else {
                    None
                }
            }
            serde_json::Value::Array(arr) => {
                let text = arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    None
                } else {
                    Some(ReasoningContent {
                        text,
                        format: ReasoningFormat::Details,
                    })
                }
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl Message {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Create an assistant message with text content.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Create an assistant message with tool calls.
    pub fn assistant_with_tool_calls(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolResult
// ---------------------------------------------------------------------------

/// Result from executing a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn ok(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Create an error tool result.
    pub fn err(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

// ---------------------------------------------------------------------------
// UsageStats / ToolErrorRecord / AgentResult
// ---------------------------------------------------------------------------

/// Token usage statistics from an LLM response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
}

/// Record of a tool error during execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolErrorRecord {
    pub tool_name: String,
    pub error: String,
    pub turn: u32,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Final result of an agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgentResult {
    pub messages: Vec<Message>,
    pub finished_naturally: bool,
    pub total_turns: u32,
    #[serde(default)]
    pub tool_errors: Vec<ToolErrorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
    /// Set when the loop stopped due to [`crate::AgentError::Interrupted`] (Python parity).
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    /// Estimated session spend in USD when cost tracking is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cost_usd: Option<f64>,
    /// Hook / plugin parity: `on_session_start` ran this run (new session, not restored prompt).
    #[serde(default, skip_serializing_if = "is_false")]
    pub session_started_hooks_fired: bool,
}

// ---------------------------------------------------------------------------
// BudgetConfig
// ---------------------------------------------------------------------------

/// Budget configuration for constraining output sizes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetConfig {
    pub max_result_size_chars: usize,
    pub max_aggregate_chars: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 1_000_000,
        }
    }
}

// ---------------------------------------------------------------------------
// LlmResponse
// ---------------------------------------------------------------------------

/// Complete response from an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmResponse {
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Streaming types
// ---------------------------------------------------------------------------

/// Delta for a function call within a streaming tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCallDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Delta for a tool call within a streaming response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionCallDelta>,
}

/// Delta content in a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// A single chunk from a streaming LLM response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<StreamDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

// ---------------------------------------------------------------------------
// Skill and Memory types (referenced by trait definitions)
// ---------------------------------------------------------------------------

/// Metadata for a skill listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMeta {
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

/// A skill definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// CommandOutput (referenced by TerminalBackend trait)
// ---------------------------------------------------------------------------

/// Output from a terminal command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_construction() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content.as_deref(), Some("Hello"));

        let msg = Message::system("You are helpful");
        assert_eq!(msg.role, MessageRole::System);
    }

    #[test]
    fn test_tool_result() {
        let ok = ToolResult::ok("call_1", "result text");
        assert!(!ok.is_error);

        let err = ToolResult::err("call_2", "failed");
        assert!(err.is_error);
    }

    #[test]
    fn test_reasoning_content_from_string() {
        let val = serde_json::Value::String("thinking...".to_string());
        let rc = ReasoningContent::from_value(&val).unwrap();
        assert_eq!(rc.text, "thinking...");
        assert_eq!(rc.format, ReasoningFormat::Simple);
    }

    #[test]
    fn test_reasoning_content_from_array() {
        let val = serde_json::json!([
            {"text": "step 1"},
            {"text": "step 2"}
        ]);
        let rc = ReasoningContent::from_value(&val).unwrap();
        assert_eq!(rc.text, "step 1\nstep 2");
        assert_eq!(rc.format, ReasoningFormat::Details);
    }

    #[test]
    fn test_serde_message_role() {
        let json = serde_json::to_string(&MessageRole::Assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
        let role: MessageRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, MessageRole::Assistant);
    }

    #[test]
    fn test_budget_config_default() {
        let bc = BudgetConfig::default();
        assert_eq!(bc.max_result_size_chars, 100_000);
        assert_eq!(bc.max_aggregate_chars, 1_000_000);
    }
}
