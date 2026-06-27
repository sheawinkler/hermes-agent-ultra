//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use hermes_auth::{exchange_refresh_token, OAuth2Endpoints};
use hermes_intelligence::get_model_context_length;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use tokio::time::sleep;

use hermes_core::{
    separate_text_and_calls, AgentError, AgentResult, BudgetConfig, LlmProvider, LlmResponse,
    Message, MessageRole, StreamChunk, ToolCall, ToolError, ToolResult, ToolSchema, UsageStats,
};

use crate::api_bridge::CodexProvider;
use crate::bedrock::{
    bedrock_runtime_base_url, resolve_bedrock_region, BedrockProvider, BEDROCK_AUTH_MARKER,
};
use crate::budget;
use crate::code_index::CodeIndex;
use crate::coding_context::resolve_runtime_mode;
use crate::context::{
    load_builtin_memory_snapshot, load_soul_md_from_home, resolve_personality, ContextManager,
    SystemPromptBuilder,
};
use crate::context_files::{load_hermes_context_files, load_workspace_context};
use crate::context_references::preprocess_context_references_async;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::lsp_context::{build_lsp_context_note, LspContextConfig};
use crate::memory_manager::{MemoryManager, StreamingContextScrubber};
use crate::plugins::{HookResult, HookType, PluginManager, ToolExecutionMiddlewareContext};
use crate::provider::{
    is_codex_chatgpt_token, is_openai_dynamic_model_alias, AnthropicProvider, GenericProvider,
    OpenAiProvider, OpenRouterProvider, OPENAI_CODEX_BASE_URL, OPENAI_CODEX_DYNAMIC_WIRE_MODEL,
};
use crate::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use crate::python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    looks_like_codex_intermediate_ack, sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, CODEX_CONTINUE_USER_MESSAGE,
};
use crate::skill_orchestrator::SkillOrchestrator;
use crate::smart_model_routing::{
    detect_api_mode_for_url, resolve_turn_route, PrimaryRuntime, ResolveTurnOutcome,
    ResolvedCheapRuntime, TurnRouteSignature,
};
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
use crate::steer::{is_formatted_steer_marker, STEER_CHANNEL_NOTE};
use crate::tool_call_args::{repair_tool_call_arguments, ToolArgumentRepair};

// Keep these includes in original item order. This split preserves the public
// `agent_loop` module namespace while separating config, runtime state, loop
// phases, helper policies, and tests for targeted maintenance.
include!("agent_loop/tool_registry.rs");
include!("agent_loop/config.rs");
include!("agent_loop/runtime_state.rs");
include!("agent_loop/agent_struct.rs");
include!("agent_loop/methods_session.rs");
include!("agent_loop/methods_hooks_memory_context.rs");
include!("agent_loop/methods_runtime_provider.rs");
include!("agent_loop/methods_prompt_llm.rs");
include!("agent_loop/methods_run.rs");
include!("agent_loop/methods_run_stream.rs");
include!("agent_loop/methods_tools_background.rs");
include!("agent_loop/finalizer_helpers.rs");

#[cfg(test)]
mod tests {
    include!("agent_loop/tests_core.rs");
    include!("agent_loop/tests_runtime.rs");
}
