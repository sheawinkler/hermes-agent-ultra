//! # hermes-core
//!
//! Foundation crate defining all shared types, traits, and error types
//! used across the hermes-agent-rust workspace.

pub mod auth_gate;
pub mod credits;
pub mod errors;
pub mod providers;
pub mod schema_sanitizer;
pub mod tool_call_parser;
pub mod tool_schema;
pub mod traits;
pub mod types;
pub mod version;

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

// Re-export schema sanitizer helpers
pub use schema_sanitizer::{
    sanitize_tool_parameters, sanitize_tool_schemas, strip_pattern_and_format, strip_slash_enum,
};

// Re-export trait definitions
pub use traits::{
    LlmProvider, MemoryProvider, PlatformAdapter, SendMessageOptions, SkillProvider,
    TerminalBackend, ToolHandler,
};

// Re-export tool call parser public API
pub use tool_call_parser::{
    format_tool_calls, get_parser, parse_tool_calls, register_parser, separate_text_and_calls,
    HermesToolCallParser, ToolCallParser,
};

// Re-export ParseMode from traits
pub use traits::ParseMode;
