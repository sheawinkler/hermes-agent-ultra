//! # hermes-core
//!
//! Foundation crate defining all shared types, traits, and error types
//! used across the hermes-agent-rust workspace.

pub mod errors;
pub mod build_info;
pub mod inbound;
pub mod time;
pub mod tool_call_parser;
pub mod tool_schema;
pub mod traits;
pub mod types;

pub mod test_env;

#[cfg(test)]
pub mod test_generators;

// Re-export all error types
pub use errors::{AgentError, ConfigError, GatewayError, ToolError};
pub use build_info::startup_commit_info;
pub use time::{
    cron_wall_offset_at, ensure_aware, ensure_aware_naive, ensure_aware_utc,
    format_conversation_started_date, format_wall_compact, format_wall_datetime, format_wall_hms,
    format_wall_ymd_hms, get_timezone, init_global_clock, now, now_utc, reset_cache,
    reset_global_clock_cache, timezone_name, tz_for_child_env, HermesClock,
};

// Re-export all core types
pub use types::{
    AgentResult, BudgetConfig, CacheControl, CacheType, CommandOutput, FunctionCall,
    FunctionCallDelta, LlmResponse, Message, MessageRole, ReasoningContent, ReasoningFormat, Skill,
    SkillMeta, StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolErrorRecord, ToolResult,
    UsageStats, PARTIAL_STREAM_STUB_ID,
};

// Re-export tool schema types
pub use tool_schema::{tool_schema, JsonSchema, ToolSchema};

pub use inbound::{
    transport_fallback_message, InboundEvent, InboundMessagePreparer, InboundPrepareContext,
};

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
