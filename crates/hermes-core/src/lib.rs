//! # hermes-core
//!
//! Foundation crate defining all shared types, traits, and error types
//! used across the hermes-agent-rust workspace.

pub mod errors;
pub mod tool_call_parser;
pub mod tool_schema;
pub mod traits;
pub mod types;

#[cfg(test)]
pub mod test_generators;

// Re-export all error types
pub use errors::{AgentError, ConfigError, GatewayError, ToolError};

// Re-export all core types
pub use types::{
    AgentResult, BudgetConfig, CacheControl, CacheType, CommandOutput, FunctionCall,
    FunctionCallDelta, LlmResponse, Message, MessageRole, ReasoningContent, ReasoningFormat, Skill,
    SkillMeta, StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolErrorRecord, ToolResult,
    UsageStats,
};

// Re-export tool schema types
pub use tool_schema::{tool_schema, JsonSchema, ToolSchema};

// Re-export trait definitions
pub use traits::{
    LlmProvider, MemoryProvider, PlatformAdapter, SkillProvider, TerminalBackend, ToolHandler,
};

// Re-export tool call parser public API
pub use tool_call_parser::{
    format_tool_calls, get_parser, parse_tool_calls, register_parser, separate_text_and_calls,
    HermesToolCallParser, ToolCallParser,
};

// Re-export ParseMode from traits
pub use traits::ParseMode;
