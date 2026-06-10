//! # hermes-core
//!
//! Foundation crate defining all shared types, traits, and error types
//! used across the hermes-agent-rust workspace.

pub mod auth_gate;
pub mod build_info;
pub mod credits;
pub mod errors;
pub mod inbound;
pub mod providers;
pub mod schema_sanitizer;
pub mod time;
pub mod token_estimator;
pub mod tool_call_parser;
pub mod tool_schema;
pub mod traits;
pub mod types;
pub mod version;

pub mod test_env;

#[cfg(test)]
pub mod test_generators;

// Re-export all error types
pub use build_info::startup_commit_info;
pub use errors::{AgentError, ConfigError, GatewayError, ToolError};
pub use time::{
    HermesClock, cron_wall_offset_at, ensure_aware, ensure_aware_naive, ensure_aware_utc,
    format_conversation_started_date, format_wall_compact, format_wall_datetime,
    format_wall_datetime_precise, format_wall_hms, format_wall_ymd_hms, get_timezone,
    init_global_clock, now, now_utc, reset_cache, reset_global_clock_cache, timezone_name,
    tz_for_child_env,
};

// Re-export all core types
pub use types::{
    AgentResult, BudgetConfig, CacheControl, CacheType, CommandOutput, FunctionCall,
    FunctionCallDelta, LlmResponse, Message, MessageRole, PARTIAL_STREAM_STUB_ID, ReasoningContent,
    ReasoningFormat, Skill, SkillMeta, StreamChunk, StreamDelta, ToolCall, ToolCallDelta,
    ToolErrorRecord, ToolResult, UsageStats,
};

// Re-export tool schema types
pub use tool_schema::{JsonSchema, ToolSchema, tool_schema};

pub use inbound::{
    InboundEvent, InboundMessagePreparer, InboundPrepareContext, transport_fallback_message,
};

// Re-export schema sanitizer helpers
pub use schema_sanitizer::{
    sanitize_tool_parameters, sanitize_tool_schemas, strip_pattern_and_format, strip_slash_enum,
};

// Re-export trait definitions
pub use traits::{
    LlmProvider, MemoryProvider, PlatformAdapter, SkillProvider, TerminalBackend, ToolHandler,
};

// Re-export tool call parser public API
pub use tool_call_parser::{
    HermesToolCallParser, ToolCallParser, format_tool_calls, get_parser, parse_tool_calls,
    register_parser, separate_text_and_calls,
};

// Re-export ParseMode from traits
pub use traits::ParseMode;

// Re-export token estimator
pub use token_estimator::{
    CharBasedEstimator, DEFAULT_TOKEN_ESTIMATOR, TokenEstimator, estimate, estimate_json,
};
