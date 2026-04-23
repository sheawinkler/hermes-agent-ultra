//! Proptest strategies for generating random instances of core types.
//!
//! These generators are used across the workspace for property-based testing.
//! Only compiled under `#[cfg(test)]`.

use proptest::prelude::*;

use crate::errors::AgentError;
use crate::types::*;

// ---------------------------------------------------------------------------
// Leaf strategies
// ---------------------------------------------------------------------------

pub fn arb_message_role() -> impl Strategy<Value = MessageRole> {
    prop_oneof![
        Just(MessageRole::System),
        Just(MessageRole::User),
        Just(MessageRole::Assistant),
        Just(MessageRole::Tool),
    ]
}

pub fn arb_cache_type() -> impl Strategy<Value = CacheType> {
    prop_oneof![Just(CacheType::Ephemeral), Just(CacheType::Persistent),]
}

pub fn arb_cache_control() -> impl Strategy<Value = CacheControl> {
    arb_cache_type().prop_map(|cache_type| CacheControl { cache_type })
}

pub fn arb_reasoning_format() -> impl Strategy<Value = ReasoningFormat> {
    prop_oneof![
        Just(ReasoningFormat::Simple),
        Just(ReasoningFormat::Details),
    ]
}

// ---------------------------------------------------------------------------
// Composite strategies
// ---------------------------------------------------------------------------

/// Alphanumeric identifier (1..32 chars), safe for tool names / IDs.
pub fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,31}".prop_map(|s| s)
}

/// Short non-empty text (1..128 chars).
pub fn arb_short_text() -> impl Strategy<Value = String> {
    ".{1,128}"
}

pub fn arb_function_call() -> impl Strategy<Value = FunctionCall> {
    (arb_identifier(), arb_json_object_string())
        .prop_map(|(name, arguments)| FunctionCall { name, arguments })
}

pub fn arb_tool_call() -> impl Strategy<Value = ToolCall> {
    (arb_identifier(), arb_function_call()).prop_map(|(id, function)| ToolCall {
        id: format!("call_{id}"),
        function,
        extra_content: None,
    })
}

/// Generate a valid JSON object string (for tool call arguments).
pub fn arb_json_object_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{}".to_string()),
        arb_identifier().prop_map(|k| format!(r#"{{"{k}": "value"}}"#)),
        (arb_identifier(), 0i64..1000).prop_map(|(k, v)| format!(r#"{{"{k}": {v}}}"#)),
        (arb_identifier(), proptest::bool::ANY).prop_map(|(k, v)| format!(r#"{{"{k}": {v}}}"#)),
    ]
}

pub fn arb_reasoning_content() -> impl Strategy<Value = ReasoningContent> {
    (arb_short_text(), arb_reasoning_format())
        .prop_map(|(text, format)| ReasoningContent { text, format })
}

pub fn arb_message() -> impl Strategy<Value = Message> {
    (
        arb_message_role(),
        proptest::option::of(arb_short_text()),
        proptest::option::of(proptest::collection::vec(arb_tool_call(), 0..3)),
        proptest::option::of(arb_identifier()),
        proptest::option::of(arb_short_text()),
        proptest::option::of(arb_short_text()),
        proptest::option::of(arb_cache_control()),
    )
        .prop_map(
            |(role, content, tool_calls, tool_call_id, name, reasoning_content, cache_control)| {
                Message {
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    name,
                    reasoning_content,
                    cache_control,
                }
            },
        )
}

pub fn arb_tool_result() -> impl Strategy<Value = ToolResult> {
    (arb_identifier(), arb_short_text(), proptest::bool::ANY).prop_map(
        |(tool_call_id, content, is_error)| ToolResult {
            tool_call_id,
            content,
            is_error,
        },
    )
}

pub fn arb_usage_stats() -> impl Strategy<Value = UsageStats> {
    (
        0u64..100_000,
        0u64..100_000,
        proptest::option::of(0.0f64..100.0),
    )
        .prop_map(
            |(prompt_tokens, completion_tokens, estimated_cost)| UsageStats {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                estimated_cost,
            },
        )
}

pub fn arb_tool_error_record() -> impl Strategy<Value = ToolErrorRecord> {
    (arb_identifier(), arb_short_text(), 0u32..100).prop_map(|(tool_name, error, turn)| {
        ToolErrorRecord {
            tool_name,
            error,
            turn,
        }
    })
}

pub fn arb_budget_config() -> impl Strategy<Value = BudgetConfig> {
    (1usize..500_000, 1usize..5_000_000).prop_map(|(max_result_size_chars, max_aggregate_chars)| {
        BudgetConfig {
            max_result_size_chars,
            max_aggregate_chars,
        }
    })
}

pub fn arb_agent_result() -> impl Strategy<Value = AgentResult> {
    (
        proptest::collection::vec(arb_message(), 0..5),
        proptest::bool::ANY,
        0u32..100,
        proptest::collection::vec(arb_tool_error_record(), 0..3),
        proptest::option::of(arb_usage_stats()),
    )
        .prop_map(
            |(messages, finished_naturally, total_turns, tool_errors, usage)| AgentResult {
                messages,
                finished_naturally,
                total_turns,
                tool_errors,
                usage,
                ..Default::default()
            },
        )
}

// ---------------------------------------------------------------------------
// Error generators
// ---------------------------------------------------------------------------

pub fn arb_agent_error() -> impl Strategy<Value = AgentError> {
    prop_oneof![
        arb_short_text().prop_map(AgentError::LlmApi),
        arb_short_text().prop_map(AgentError::ToolExecution),
        arb_short_text().prop_map(AgentError::Config),
        arb_short_text().prop_map(AgentError::Gateway),
        arb_short_text().prop_map(AgentError::Timeout),
        Just(AgentError::MaxTurnsExceeded),
        arb_short_text().prop_map(AgentError::InvalidToolCall),
        Just(AgentError::ContextTooLong),
        proptest::option::of(0u64..3600)
            .prop_map(|retry_after_secs| AgentError::RateLimited { retry_after_secs }),
        arb_short_text().prop_map(AgentError::AuthFailed),
        arb_short_text().prop_map(AgentError::Io),
        proptest::option::of(arb_short_text())
            .prop_map(|message| AgentError::Interrupted { message }),
    ]
}
