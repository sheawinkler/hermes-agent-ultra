//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use hermes_auth::{exchange_refresh_token, OAuth2Endpoints};
use hermes_intelligence::get_model_context_length;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;
use tokio::time::sleep;

use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, ToolCall, ToolError, ToolResult, ToolSchema, UsageStats,
};

use crate::api_bridge::CodexProvider;
use crate::budget;
use crate::code_index::CodeIndex;
use crate::context::{
    load_builtin_memory_snapshot, load_soul_md, resolve_personality, ContextManager,
    SystemPromptBuilder,
};
use crate::context_files::{load_hermes_context_files, load_workspace_context};
use crate::context_references::preprocess_context_references_async;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::lsp_context::{build_lsp_context_note, LspContextConfig};
use crate::memory_manager::MemoryManager;
use crate::plugins::{HookResult, HookType, PluginManager};
use crate::provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
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

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// A single tool entry in the registry.
#[derive(Clone)]
pub struct ToolEntry {
    /// The tool's JSON Schema descriptor.
    pub schema: ToolSchema,
    /// A handler function: takes a JSON Value and returns the tool output string.
    pub handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("schema", &self.schema)
            .field("handler", &"<function>")
            .finish()
    }
}

/// A simple registry mapping tool names to their schemas and handlers.
///
/// The full-featured implementation lives in `hermes-tools`; this minimal
/// version exists so the agent loop can be tested and used independently.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        self.tools
            .insert(name.into(), ToolEntry { schema, handler });
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    /// Return all registered tool schemas.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|e| e.schema.clone()).collect()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

/// Retry / failover configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum retries before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base delay for exponential backoff (ms).
    #[serde(default = "default_base_delay_ms")]
    pub base_delay_ms: u64,
    /// Maximum backoff cap (ms).
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,
    /// Optional fallback model identifier (tried after all retries on the primary model fail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
}

fn default_max_retries() -> u32 {
    3
}
fn default_base_delay_ms() -> u64 {
    1000
}
fn default_max_delay_ms() -> u64 {
    30_000
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            fallback_model: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional external process command for provider runtimes (Python parity metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Optional argv tail for external process runtimes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// OAuth2 token endpoint for refresh flows (provider config centre).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_token_url: Option<String>,
    /// OAuth2 client_id for refresh flows (provider config centre).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthStoreCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

const MEMORY_GUIDANCE: &str = "You have persistent memory across sessions. Save durable facts using the memory tool: user preferences, environment details, tool quirks, and stable conventions. Memory is injected into every turn, so keep it compact and focused on facts that will still matter later. Prioritize what reduces future user steering. Do NOT save task progress, session outcomes, completed-work logs, or temporary TODO state to memory.";

const SESSION_SEARCH_GUIDANCE: &str = "When the user references something from a past conversation or you suspect relevant cross-session context exists, use session_search to recall it before asking them to repeat themselves.";

const SKILLS_GUIDANCE: &str = "After completing a complex task (5+ tool calls), fixing a tricky error, or discovering a non-trivial workflow, save the approach as a skill with skill_manage so you can reuse it next time. When using a skill and finding it outdated or incomplete, patch it immediately with skill_manage(action='patch').";
const OAUTH_REFRESH_BACKOFF_SECS: u64 = 60;

// Python `AIAgent._MEMORY_REVIEW_PROMPT` / `_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT` (v2026.4.13)
const MEMORY_REVIEW_PROMPT: &str = "Review the conversation above and consider saving to memory if appropriate.\n\n\
Focus on:\n\
1. Has the user revealed things about themselves — their persona, desires, preferences, or personal details worth remembering?\n\
2. Has the user expressed expectations about how you should behave, their work style, or ways they want you to operate?\n\n\
If something stands out, save it using the memory tool. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const SKILL_REVIEW_PROMPT: &str =
    "Review the conversation above and consider saving or updating a skill if appropriate.\n\n\
Focus on: was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome?\n\n\
If a relevant skill already exists, update it with what you learned. \
Otherwise, create a new skill if the approach is reusable.\n\
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const COMBINED_REVIEW_PROMPT: &str = "Review the conversation above and consider two things:\n\n\
**Memory**: Has the user revealed things about themselves — their persona, \
desires, preferences, or personal details? Has the user expressed expectations \
about how you should behave, their work style, or ways they want you to operate? \
If so, save using the memory tool.\n\n\
**Skills**: Was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome? If a relevant skill \
already exists, update it. Otherwise, create a new one if the approach is reusable.\n\n\
Only act if there's something genuinely worth saving. \
If nothing stands out, just say 'Nothing to save.' and stop.";

const TOOL_USE_ENFORCEMENT_GUIDANCE: &str = "# Tool-use enforcement\nYou MUST use your tools to take action. Do not describe what you would do without actually doing it. When you say you will perform an action, make the corresponding tool call in the same response. Every response should either (a) contain tool calls that make progress, or (b) deliver a final result.";

const OPENAI_MODEL_EXECUTION_GUIDANCE: &str = "# Execution discipline (OpenAI)\nUse tools whenever they improve correctness, completeness, or grounding. Do not stop early when another tool call would materially improve the result. Verify outcomes before declaring completion.";

const GOOGLE_MODEL_OPERATIONAL_GUIDANCE: &str = "# Operational guidance (Google)\nBe concise and execution-first. Prefer absolute paths, parallel tool calls when safe, and verify each substantive change.";

/// Configuration for the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of LLM → tool → LLM iterations.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Budget settings for truncating tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Model identifier (e.g. "gpt-4o", "claude-3-5-sonnet").
    #[serde(default = "default_model")]
    pub model: String,

    /// API mode — selects the request format for the LLM provider.
    #[serde(default)]
    pub api_mode: ApiMode,

    /// Retry / failover configuration.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Optional system prompt prepended to every conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Optional personality overlay appended to the system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Extra JSON body fields forwarded to the provider on every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,

    /// Whether to use streaming mode by default.
    #[serde(default)]
    pub stream: bool,

    /// Suppress user-facing lifecycle/status output for this agent session.
    /// Intended for child/delegated/background agents to match Python quiet_mode semantics.
    #[serde(default)]
    pub quiet_mode: bool,

    /// Temperature for LLM sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens for LLM completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Maximum number of concurrent delegate_task tool calls.
    #[serde(default = "default_max_concurrent_delegates")]
    pub max_concurrent_delegates: u32,

    /// Flush memories every N turns.
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,

    /// Session identifier — used for memory and persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// HERMES_HOME path — used by memory plugins for config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes_home: Option<String>,

    /// Skip memory integration even if a MemoryManager is provided.
    #[serde(default)]
    pub skip_memory: bool,

    /// Skip auto-injection of workspace/personal context files in system prompt
    /// assembly (SOUL.md, AGENTS.md, etc.).
    #[serde(default)]
    pub skip_context_files: bool,

    /// Optional cheap-vs-strong per-turn routing.
    #[serde(default)]
    pub smart_model_routing: SmartModelRoutingConfig,

    /// Provider hint (e.g. "openai", "anthropic", "openrouter").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Optional platform hint key (e.g. "cli", "telegram", "discord").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// Include session_id in system prompt timestamp block.
    #[serde(default)]
    pub pass_session_id: bool,

    /// Runtime provider credentials/endpoints keyed by provider name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub runtime_providers: HashMap<String, RuntimeProviderConfig>,

    /// Ephemeral system prompt appended at API-call time only.
    /// This is intentionally not persisted in context history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_system_prompt: Option<String>,

    /// Session-level hard spend limit in USD. When reached, the loop trips
    /// the cost gate and returns early with a summary system message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,

    /// Ratio (0.0-1.0) at which to proactively degrade to a cheaper model.
    #[serde(default = "default_cost_guard_degrade_at_ratio")]
    pub cost_guard_degrade_at_ratio: f64,

    /// Optional explicit cheaper model to use after crossing the degrade ratio.
    /// If unset, falls back to `retry.fallback_model` then a built-in cheap default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_guard_degrade_model: Option<String>,

    /// Optional per-million-token prompt price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cost_per_million_usd: Option<f64>,

    /// Optional per-million-token completion price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_cost_per_million_usd: Option<f64>,

    /// Auto-checkpoint interval in turns. `0` disables automatic checkpoints.
    #[serde(default = "default_checkpoint_interval_turns")]
    pub checkpoint_interval_turns: u32,

    /// If a single turn generates at least this many tool errors, rollback
    /// to the latest checkpoint and continue. `0` disables rollback.
    #[serde(default = "default_rollback_on_tool_error_threshold")]
    pub rollback_on_tool_error_threshold: u32,

    /// External process / ACP command (Python primary `command` in `resolve_turn_route` signature).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_command: Option<String>,

    /// External process / ACP argv tail (Python primary `args`).
    #[serde(default)]
    pub acp_args: Vec<String>,

    /// User-turn cadence for memory background review ticks (`0` disables interval).
    #[serde(default = "default_memory_nudge_interval")]
    pub memory_nudge_interval: u32,

    /// Tool-loop iterations without `skill_manage` before skill background review (`0` disables).
    #[serde(default = "default_skill_creation_nudge_interval")]
    pub skill_creation_nudge_interval: u32,

    /// Run Python-style background memory/skill review after a session (extra LLM calls).
    /// Python has no master off-switch; matches default-on (`cli-config.yaml` can override via `agent`).
    #[serde(default = "default_background_review_enabled")]
    pub background_review_enabled: bool,

    /// Emit background review metrics snapshots (`debug` tracing only).
    /// Child sessions disable this for stricter quiet-mode parity.
    #[serde(default = "default_background_review_metrics_enabled")]
    pub background_review_metrics_enabled: bool,

    /// Exact system prompt from SQLite when continuing a session (stable Anthropic prefix cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_system_prompt: Option<String>,

    /// Progress ratio (0–1) at which Python emits a *caution* budget nudge (`_budget_caution_threshold`).
    #[serde(default = "default_budget_caution_threshold")]
    pub budget_caution_threshold: f64,

    /// Progress ratio (0–1) at which Python emits an urgent budget warning (`_budget_warning_threshold`).
    #[serde(default = "default_budget_warning_threshold")]
    pub budget_warning_threshold: f64,

    /// When false, skip `_get_budget_warning`-style injection entirely.
    #[serde(default = "default_budget_pressure_enabled")]
    pub budget_pressure_enabled: bool,

    /// Retries when the model returns empty assistant text with no tools (Python `_empty_content_retries`, max 3).
    #[serde(default = "default_empty_content_max_retries")]
    pub empty_content_max_retries: u32,

    /// Thinking-only prefill rounds (Python `_thinking_prefill_retries`, max 2).
    #[serde(default = "default_thinking_prefill_max_retries")]
    pub thinking_prefill_max_retries: u32,

    /// Additional retries for streaming transport failures during one turn's
    /// streaming collection path.
    #[serde(default = "default_stream_read_max_retries")]
    pub stream_read_max_retries: u32,

    /// Run one compression pass before the first LLM call if context is already over threshold.
    #[serde(default = "default_preflight_context_compress")]
    pub preflight_context_compress: bool,

    /// Optional replacement for the **persisted** last user message (API-facing user text may differ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_user_message: Option<String>,

    /// Max retries when the model emits unknown tool names.
    #[serde(default = "default_invalid_tool_call_max_retries")]
    pub invalid_tool_call_max_retries: u32,

    /// Max retries when tool call arguments are invalid JSON.
    #[serde(default = "default_invalid_tool_json_max_retries")]
    pub invalid_tool_json_max_retries: u32,

    /// Max retries when streaming assembles truncated tool arguments (`finish_reason=length` parity).
    #[serde(default = "default_truncated_tool_call_max_retries")]
    pub truncated_tool_call_max_retries: u32,

    /// Enable always-on workspace code indexing + repo-map prompt injection.
    #[serde(default = "default_code_index_enabled")]
    pub code_index_enabled: bool,

    /// Maximum files included in repo-map prompt rendering.
    #[serde(default = "default_code_index_max_files")]
    pub code_index_max_files: usize,

    /// Maximum symbols included in repo-map prompt rendering.
    #[serde(default = "default_code_index_max_symbols")]
    pub code_index_max_symbols: usize,

    /// Enable LSP-style context injection after file tool calls.
    #[serde(default = "default_lsp_context_enabled")]
    pub lsp_context_enabled: bool,

    /// Maximum character budget for injected LSP context block.
    #[serde(default = "default_lsp_context_max_chars")]
    pub lsp_context_max_chars: usize,
}

fn default_max_turns() -> u32 {
    30
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

fn default_max_concurrent_delegates() -> u32 {
    1
}

fn default_memory_flush_interval() -> u32 {
    5
}

fn default_cost_guard_degrade_at_ratio() -> f64 {
    0.8
}

fn default_checkpoint_interval_turns() -> u32 {
    3
}

fn default_rollback_on_tool_error_threshold() -> u32 {
    3
}

fn default_memory_nudge_interval() -> u32 {
    10
}

fn default_skill_creation_nudge_interval() -> u32 {
    10
}

fn default_background_review_enabled() -> bool {
    true
}

fn default_background_review_metrics_enabled() -> bool {
    true
}

fn default_budget_caution_threshold() -> f64 {
    0.7
}

fn default_budget_warning_threshold() -> f64 {
    0.9
}

fn default_budget_pressure_enabled() -> bool {
    true
}

fn default_empty_content_max_retries() -> u32 {
    3
}

fn default_thinking_prefill_max_retries() -> u32 {
    2
}

fn default_stream_read_max_retries() -> u32 {
    2
}

fn default_preflight_context_compress() -> bool {
    true
}

fn default_invalid_tool_call_max_retries() -> u32 {
    3
}

fn default_invalid_tool_json_max_retries() -> u32 {
    3
}

fn default_truncated_tool_call_max_retries() -> u32 {
    3
}

fn default_code_index_enabled() -> bool {
    true
}

fn default_code_index_max_files() -> usize {
    32
}

fn default_code_index_max_symbols() -> usize {
    160
}

fn default_lsp_context_enabled() -> bool {
    true
}

fn default_lsp_context_max_chars() -> usize {
    2_800
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            budget: BudgetConfig::default(),
            model: default_model(),
            api_mode: ApiMode::default(),
            retry: RetryConfig::default(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            quiet_mode: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            hermes_home: None,
            skip_memory: false,
            skip_context_files: false,
            smart_model_routing: SmartModelRoutingConfig::default(),
            provider: None,
            platform: None,
            pass_session_id: false,
            runtime_providers: HashMap::new(),
            ephemeral_system_prompt: None,
            max_cost_usd: None,
            cost_guard_degrade_at_ratio: default_cost_guard_degrade_at_ratio(),
            cost_guard_degrade_model: None,
            prompt_cost_per_million_usd: None,
            completion_cost_per_million_usd: None,
            checkpoint_interval_turns: default_checkpoint_interval_turns(),
            rollback_on_tool_error_threshold: default_rollback_on_tool_error_threshold(),
            acp_command: None,
            acp_args: Vec::new(),
            memory_nudge_interval: default_memory_nudge_interval(),
            skill_creation_nudge_interval: default_skill_creation_nudge_interval(),
            background_review_enabled: default_background_review_enabled(),
            background_review_metrics_enabled: default_background_review_metrics_enabled(),
            stored_system_prompt: None,
            budget_caution_threshold: default_budget_caution_threshold(),
            budget_warning_threshold: default_budget_warning_threshold(),
            budget_pressure_enabled: default_budget_pressure_enabled(),
            empty_content_max_retries: default_empty_content_max_retries(),
            thinking_prefill_max_retries: default_thinking_prefill_max_retries(),
            stream_read_max_retries: default_stream_read_max_retries(),
            preflight_context_compress: default_preflight_context_compress(),
            persist_user_message: None,
            invalid_tool_call_max_retries: default_invalid_tool_call_max_retries(),
            invalid_tool_json_max_retries: default_invalid_tool_json_max_retries(),
            truncated_tool_call_max_retries: default_truncated_tool_call_max_retries(),
            code_index_enabled: default_code_index_enabled(),
            code_index_max_files: default_code_index_max_files(),
            code_index_max_symbols: default_code_index_max_symbols(),
            lsp_context_enabled: default_lsp_context_enabled(),
            lsp_context_max_chars: default_lsp_context_max_chars(),
        }
    }
}

// ---------------------------------------------------------------------------
// TurnMetrics
// ---------------------------------------------------------------------------

/// Timing and usage metrics for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnMetrics {
    /// Wall-clock time spent waiting for the LLM API, in milliseconds.
    pub api_time_ms: u64,
    /// Wall-clock time spent executing tools, in milliseconds.
    pub tool_time_ms: u64,
    /// Token usage for this turn (if reported by the provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

// ---------------------------------------------------------------------------
// Evolution counters (Python `_turns_since_memory` / `_iters_since_skill`)
// ---------------------------------------------------------------------------

/// Session-scoped counters for memory / skill nudges (mirrors Python `AIAgent` fields).
#[derive(Debug, Default)]
pub struct EvolutionCounters {
    pub turns_since_memory: u32,
    pub iters_since_skill: u32,
}

// ---------------------------------------------------------------------------
// AgentLoop
// ---------------------------------------------------------------------------

/// Callbacks invoked during tool execution for progress reporting.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Called when the LLM is "thinking" (reasoning tokens).
    pub on_thinking: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a tool call begins.
    pub on_tool_start: Option<Box<dyn Fn(&str, &Value) + Send + Sync>>,
    /// Called when a tool call finishes.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str) + Send + Sync>>,
    /// Called for each stream delta.
    pub on_stream_delta: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called after each completed LLM step (full response assembled).
    pub on_step_complete: Option<Box<dyn Fn(u32) + Send + Sync>>,
    /// Called when background memory/skill review completes or fails.
    ///
    /// Payload is a user-friendly summary string suitable for direct UI output.
    pub background_review_callback: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called for lifecycle/status notices (context pressure, retries, etc.).
    pub status_callback: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
}

/// Classify an API error for retry/failover decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Retryable,
    RateLimit,
    ContextOverflow,
    Auth,
    Fatal,
}

fn has_ssl_transient_phrase(lower: &str) -> bool {
    lower.contains("bad record mac")
        || lower.contains("ssl alert")
        || lower.contains("tls alert")
        || lower.contains("ssl handshake failure")
        || lower.contains("tlsv1 alert")
        || lower.contains("sslv3 alert")
        || lower.contains("bad_record_mac")
        || lower.contains("ssl_alert")
        || lower.contains("tls_alert")
        || lower.contains("tls_alert_internal_error")
        || lower.contains("[ssl:")
}

fn maybe_nous_401_diagnostic(
    provider_hint: &str,
    err: &str,
    hermes_home: Option<&str>,
) -> Option<String> {
    let provider = provider_hint.trim().to_ascii_lowercase();
    if !provider.starts_with("nous") {
        return None;
    }
    let lower = err.to_ascii_lowercase();
    let is_auth_401 =
        lower.contains("401") || lower.contains("unauthorized") || lower.contains("authentication");
    if !is_auth_401 {
        return None;
    }

    let response = err.replace('\n', " ");
    let response_snippet = if response.len() > 200 {
        format!("{}...", &response[..200])
    } else {
        response
    };
    let auth_json = hermes_home
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermes-agent-ultra")
        })
        .join("auth.json");

    Some(format!(
        "Nous 401 — Portal authentication failed.\n\
         Response: {response_snippet}\n\
         Most likely: Portal OAuth expired, account out of credits, or agent key revoked.\n\
         Troubleshooting:\n\
           - Re-authenticate: hermes auth login nous\n\
           - Check credits / billing: https://portal.nousresearch.com\n\
           - Verify stored credentials: {}\n\
           - Switch providers temporarily: /model <model> --provider openrouter",
        auth_json.display()
    ))
}

fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    let model_not_found = lower.contains("model not found")
        || lower.contains("invalid model")
        || lower.contains("no such model")
        || lower.contains("unknown model");
    let openrouter_privacy_guardrail =
        lower.contains("privacy guardrail") || lower.contains("openrouter privacy");

    if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many") {
        ErrorClass::RateLimit
    } else if lower.contains("404") || lower.contains("not found") {
        if model_not_found || openrouter_privacy_guardrail {
            ErrorClass::Fatal
        } else {
            ErrorClass::Retryable
        }
    } else if lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("context_length_exceeded")
    {
        ErrorClass::ContextOverflow
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
    {
        ErrorClass::Auth
    } else if has_ssl_transient_phrase(&lower) {
        ErrorClass::Retryable
    } else if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("disconnected")
        || lower.contains("overloaded")
    {
        ErrorClass::Retryable
    } else {
        ErrorClass::Fatal
    }
}

fn is_transient_stream_error(err: &AgentError) -> bool {
    fn has_transient_phrase(msg: &str) -> bool {
        let lower = msg.to_lowercase();
        lower.contains("timeout")
            || lower.contains("connection")
            || lower.contains("disconnected")
            || lower.contains("remoteprotocol")
            || lower.contains("remote protocol")
            || lower.contains("network error")
            || lower.contains("broken pipe")
            || lower.contains("connection reset")
            || lower.contains("connection closed")
            || lower.contains("connection lost")
            || lower.contains("upstream connect error")
            || lower.contains("stream read error")
            || has_ssl_transient_phrase(&lower)
    }

    match err {
        AgentError::Timeout(_) => true,
        AgentError::LlmApi(msg)
        | AgentError::Gateway(msg)
        | AgentError::Io(msg)
        | AgentError::ToolExecution(msg)
        | AgentError::Config(msg)
        | AgentError::AuthFailed(msg)
        | AgentError::InvalidToolCall(msg) => has_transient_phrase(msg),
        AgentError::RateLimited { .. } => true,
        AgentError::Interrupted { .. }
        | AgentError::MaxTurnsExceeded
        | AgentError::ContextTooLong => false,
    }
}

/// Compute jittered exponential backoff delay.
fn jittered_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(max_ms);
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter / 2) + (rand_u64_range(0, jitter.max(1)));
    Duration::from_millis(delay)
}

fn rand_u64_range(min: u64, max: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let h = hasher.finish();
    if max <= min {
        min
    } else {
        min + h % (max - min)
    }
}

/// Result of collecting one streaming completion (may end with user interrupt).
enum StreamCollectOutcome {
    Complete(LlmResponse),
    Interrupted(LlmResponse),
}

#[derive(Debug, Clone, Copy)]
struct TurnGovernor {
    max_tokens: Option<u32>,
    tool_concurrency: usize,
    pressure: f64,
}

#[derive(Debug, Clone)]
struct ReplayRecorder {
    path: Option<PathBuf>,
}

impl ReplayRecorder {
    fn for_session(config: &AgentConfig, session_id: &str) -> Self {
        let enabled = std::env::var("HERMES_REPLAY_ENABLED")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if !enabled {
            return Self { path: None };
        }
        let root = config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(home).join(".hermes-agent-ultra"))
            })
            .unwrap_or_else(|| PathBuf::from(".hermes-agent-ultra"));
        let dir = root.join("logs").join("replay");
        if std::fs::create_dir_all(&dir).is_err() {
            return Self { path: None };
        }
        let sid = if session_id.trim().is_empty() {
            format!("session-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"))
        } else {
            session_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        Self {
            path: Some(dir.join(format!("{sid}.jsonl"))),
        }
    }

    fn record(&self, event: &str, payload: Value) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let mut redacted = payload;
        redact_json_value(&mut redacted);
        let line = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "event": event,
            "payload": redacted,
        })
        .to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn replay_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("api_key")
        || k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("authorization")
        || k.contains("cookie")
        || k.contains("session")
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if replay_sensitive_key(k) {
                    *v = Value::String("[redacted]".to_string());
                } else {
                    redact_json_value(v);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json_value(v);
            }
        }
        _ => {}
    }
}

fn governor_enabled() -> bool {
    std::env::var("HERMES_PERFORMANCE_GOVERNOR")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

fn governor_tool_concurrency_base() -> usize {
    std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

fn governor_for_turn(
    config: &AgentConfig,
    ctx: &ContextManager,
    requested_tools: usize,
) -> TurnGovernor {
    let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8).max(1.0);
    let pressure = (ctx.total_chars() as f64 / threshold).max(0.0);
    let enabled = governor_enabled();

    let max_tokens = if enabled {
        config.max_tokens.map(|base| {
            if pressure >= 0.95 {
                base.saturating_div(4).max(64)
            } else if pressure >= 0.85 {
                base.saturating_div(2).max(128)
            } else {
                base
            }
        })
    } else {
        config.max_tokens
    };

    let base_concurrency = governor_tool_concurrency_base();
    let mut tool_concurrency = if enabled {
        if pressure >= 0.95 {
            base_concurrency.min(2)
        } else if pressure >= 0.85 {
            base_concurrency.min(4)
        } else {
            base_concurrency
        }
    } else {
        base_concurrency
    };
    if requested_tools > 0 {
        tool_concurrency = tool_concurrency.min(requested_tools).max(1);
    }

    TurnGovernor {
        max_tokens,
        tool_concurrency,
        pressure,
    }
}

/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional memory manager for prefetch/sync/tool routing.
    pub memory_manager: Option<Arc<std::sync::Mutex<MemoryManager>>>,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
    /// Primary LLM credential pool (Python `primary["credential_pool"]` / runtime pool).
    pub primary_credential_pool: Option<Arc<CredentialPool>>,
    /// Memory/skill nudge counters (persist for the lifetime of this `AgentLoop`).
    pub evolution_counters: Arc<Mutex<EvolutionCounters>>,
    /// Backoff window for oauth refresh failures (avoid hammering token endpoints every turn).
    oauth_refresh_backoff: Arc<Mutex<HashMap<String, Instant>>>,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    sub_agent_orchestrator: Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Always-on workspace code index + repo-map source.
    code_index: Option<Arc<Mutex<CodeIndex>>>,
    /// LSP-style context injection controls.
    lsp_context: LspContextConfig,
}

#[derive(Debug, Clone)]
struct TurnRuntimeRoute {
    model: String,
    provider: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    api_mode: Option<ApiMode>,
    command: Option<String>,
    args: Vec<String>,
    credential_pool: Option<Arc<CredentialPool>>,
    /// When true (default), merge with [`AgentLoop::primary_credential_pool`] if route pool is unset.
    credential_pool_fallback: bool,
    route_label: Option<String>,
    routing_reason: Option<String>,
    signature: TurnRouteSignature,
}

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermes_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        let Some(ref sid) = config.session_id else {
            return Ok(());
        };
        if sid.trim().is_empty() {
            return Ok(());
        }
        let sp = crate::session_persistence::SessionPersistence::new(hermes_home);
        sp.ensure_db()?;
        if let Some(prompt) = sp.get_system_prompt(sid)? {
            config.stored_system_prompt = Some(prompt);
        }
        Ok(())
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
    ) -> Vec<Message> {
        let mut msgs = ctx.get_messages().to_vec();
        if let (Some(idx), Some(override_text)) = (
            persist_user_idx,
            self.config.persist_user_message.as_deref(),
        ) {
            if let Some(msg) = msgs.get_mut(idx) {
                if msg.role == MessageRole::User {
                    msg.content = Some(override_text.to_string());
                }
            }
        }
        msgs
    }

    fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermes_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
    ) -> AgentResult {
        self.memory_on_session_end(ctx.get_messages());
        AgentResult {
            messages: self.messages_for_persisted_result(ctx, persist_user_idx),
            finished_naturally: false,
            total_turns,
            tool_errors: tool_errors.to_vec(),
            usage: accumulated_usage,
            interrupted: true,
            session_cost_usd: Some(session_cost_usd),
            session_started_hooks_fired,
        }
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
        }
    }

    fn init_code_index(config: &AgentConfig) -> Option<Arc<Mutex<CodeIndex>>> {
        if !config.code_index_enabled {
            return None;
        }
        let workspace_root = std::env::var("TERMINAL_CWD")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if !workspace_root.exists() {
            return None;
        }
        let mut index = CodeIndex::default_for_workspace(workspace_root);
        let _ = index.ensure_fresh();
        Some(Arc::new(Mutex::new(index)))
    }

    fn build_lsp_context_config(config: &AgentConfig) -> LspContextConfig {
        let mut cfg = LspContextConfig::from_env();
        cfg.enabled = cfg.enabled && config.lsp_context_enabled;
        cfg.max_chars = config.lsp_context_max_chars.max(400);
        cfg
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the memory manager.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        self.memory_manager = Some(mm);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    // -- Plugin hook helpers ------------------------------------------------

    fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        if let Some(ref pm) = self.plugin_manager {
            if let Ok(pm) = pm.lock() {
                return pm.invoke_hook(hook, ctx_val);
            }
        }
        Vec::new()
    }

    fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        for r in results {
            if let HookResult::InjectContext(text) = r {
                ctx.add_message(Message::system(text));
            }
        }
    }

    // -- Memory helpers ----------------------------------------------------

    fn memory_prefetch(&self, query: &str, session_id: &str) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.prefetch_all(query, session_id);
            }
        }
        String::new()
    }

    fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        if self.config.skip_memory {
            return;
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                mm.sync_all(user, assistant, session_id);
                if !user.trim().is_empty() {
                    mm.queue_prefetch_all(user, session_id);
                }
            }
        }
    }

    fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
        if tc.function.name != "memory" {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_lowercase();
        if action != "add" && action != "replace" && action != "remove" {
            return None;
        }
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("memory")
            .to_string();
        let content = if action == "remove" {
            args.get("old_text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        } else {
            args.get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        Some((action, target, content))
    }

    fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mut mm) = mm.lock() else {
            return;
        };
        for result in results {
            if result.is_error {
                continue;
            }
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((action, target, content)) = Self::memory_write_event_from_tool_call(tc)
            else {
                continue;
            };
            mm.on_memory_write(&action, &target, &content);
        }
    }

    fn delegation_event_from_tool_result(
        tc: &ToolCall,
        result: &ToolResult,
    ) -> Option<(String, String)> {
        if tc.function.name != "delegate_task" || result.is_error {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        let sub_agent_id = serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|v| {
                v.get("sub_agent_id")
                    .and_then(|id| id.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
            .unwrap_or_default();

        Some((task, sub_agent_id))
    }

    fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        for result in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((task, sub_agent_id)) = Self::delegation_event_from_tool_result(tc, result)
            else {
                continue;
            };
            mm.on_delegation(&task, &sub_agent_id);
        }
    }

    fn memory_on_turn_start(&self, turn: u32, message: &str) {
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mut mm) = mm.lock() {
                mm.on_turn_start(turn, message);
            }
        }
    }

    fn memory_system_prompt(&self) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.build_system_prompt();
            }
        }
        String::new()
    }

    fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        if self.config.skip_memory {
            return None;
        }
        let Some(ref mm) = self.memory_manager else {
            return None;
        };
        let Ok(mm) = mm.lock() else {
            return None;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let note = mm.on_pre_compress(&as_values);
        if note.trim().is_empty() {
            None
        } else {
            Some(note)
        }
    }

    fn memory_on_session_end(&self, messages: &[Message]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        mm.on_session_end(&as_values);
    }

    fn code_index_repo_map_block(&self) -> Option<String> {
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        let rendered = idx.render_repo_map(
            Some(self.config.code_index_max_files),
            Some(self.config.code_index_max_symbols),
        );
        if rendered.trim().is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    fn lsp_context_note(&self, tool_calls: &[ToolCall], results: &[ToolResult]) -> Option<String> {
        if !self.lsp_context.enabled {
            return None;
        }
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        build_lsp_context_note(tool_calls, results, &mut idx, &self.lsp_context)
    }

    fn should_inject_tool_enforcement(&self, model: &str) -> bool {
        let model_lower = model.to_lowercase();
        ["gpt", "codex", "gemini", "gemma", "grok"]
            .iter()
            .any(|p| model_lower.contains(p))
    }

    fn platform_hint_text(&self) -> Option<&'static str> {
        let platform_key = self
            .config
            .platform
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        match platform_key.as_str() {
            "cli" => Some("You are a CLI AI Agent. Prefer concise plain text output suitable for terminals."),
            "telegram" | "discord" | "slack" => {
                Some("You are responding on a chat platform. Keep responses concise and avoid heavy formatting.")
            }
            "email" => Some("You are responding over email. Use clear structure and complete sentences."),
            "sms" => Some("You are responding over SMS. Keep responses short and high-signal."),
            _ => None,
        }
    }

    fn effective_provider_for_prompt(&self, model: &str) -> Option<String> {
        if let Some(ref p) = self.config.provider {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        model
            .split_once(':')
            .map(|(provider, _)| provider.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn skills_system_prompt(&self, tool_names: &HashSet<&str>) -> Option<String> {
        let has_skills_tools = ["skills_list", "skill_view", "skill_manage"]
            .iter()
            .any(|t| tool_names.contains(*t));
        if !has_skills_tools {
            return None;
        }
        let mut orch = SkillOrchestrator::default_dir();
        let commands = orch.scan_skill_commands();
        if commands.is_empty() {
            return Some(
                "## Skills (mandatory)\nSkills tools are enabled. Use `skills_list` to discover available skills and `skill_view` before applying one."
                    .to_string(),
            );
        }
        let mut rows: Vec<_> = commands.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let mut body = String::from(
            "## Skills (mandatory)\nBefore replying, check whether an existing skill applies. If yes, inspect it with `skill_view` and follow it.\n<available_skills>\n",
        );
        for (cmd, info) in rows.into_iter().take(80) {
            body.push_str(&format!(
                "- {}: {} ({})\n",
                cmd,
                info.name,
                info.description.trim()
            ));
        }
        body.push_str("</available_skills>");
        Some(body)
    }

    fn context_files_prompt(&self) -> Option<String> {
        if self.config.skip_context_files {
            return None;
        }
        let cwd = std::env::var("TERMINAL_CWD")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });

        let mut sections = Vec::new();
        if let Some(workspace) = load_workspace_context(&cwd) {
            sections.push(format!("## Workspace Context\n{}", workspace));
        }

        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HERMES_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
            })
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| std::path::PathBuf::from(".hermes"));

        let personal_ctx = load_hermes_context_files(&hermes_home);
        if !personal_ctx.trim().is_empty() {
            sections.push(format!("## Personal Context\n{}", personal_ctx));
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

    fn extract_provider_and_model<'a>(&self, model: &'a str) -> (String, &'a str) {
        if let Some((p, m)) = model.split_once(':') {
            let p = p.trim();
            let m = m.trim();
            if !p.is_empty() && !m.is_empty() {
                return (p.to_string(), m);
            }
        }
        let fallback_provider = self
            .config
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("openai")
            .to_string();
        (fallback_provider, model)
    }

    fn resolve_runtime_api_key(
        &self,
        provider: &str,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
    ) -> Option<String> {
        if provider == "copilot-acp" {
            return Some("copilot-acp".to_string());
        }
        if let Some(token) = self.resolve_oauth_store_api_key(provider) {
            return Some(token);
        }
        if let Some(key) = explicit_api_key.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(key.to_string());
        }
        if let Some(env_name) = api_key_env_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(v) = std::env::var(env_name) {
                if !v.trim().is_empty() {
                    return Some(v);
                }
            }
        }
        if let Some(cfg) = self.config.runtime_providers.get(provider) {
            if let Some(ref key) = cfg.api_key {
                let trimmed = key.trim();
                if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}'))
                {
                    if let Ok(v) = std::env::var(env_ref) {
                        if !v.trim().is_empty() {
                            return Some(v);
                        }
                    }
                } else if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            if let Some(env_name) = cfg
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Ok(v) = std::env::var(env_name) {
                    if !v.trim().is_empty() {
                        return Some(v);
                    }
                }
            }
        }
        if matches!(provider, "openai" | "codex" | "openai-codex") {
            return std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        if provider == "stepfun" {
            return std::env::var("HERMES_STEPFUN_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        match provider {
            "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
                .filter(|v| !v.trim().is_empty()),
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            }
            "openrouter" => std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "qwen" | "qwen-oauth" => std::env::var("DASHSCOPE_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "kimi" | "moonshot" => std::env::var("MOONSHOT_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "minimax" => std::env::var("MINIMAX_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "nous" => std::env::var("NOUS_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "copilot" | "copilot-acp" => std::env::var("GITHUB_COPILOT_TOKEN")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            _ => None,
        }
    }

    fn resolve_runtime_base_url(
        &self,
        provider: &str,
        route_base_url: Option<&str>,
    ) -> Option<String> {
        if let Some(b) = route_base_url.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(b.to_string());
        }
        self.config
            .runtime_providers
            .get(provider)
            .and_then(|c| c.base_url.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if provider == "copilot-acp" {
                    std::env::var("COPILOT_ACP_BASE_URL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("acp://copilot".to_string()))
                } else if provider == "openai-codex" || provider == "codex" {
                    Some("https://api.openai.com/v1".to_string())
                } else if provider == "qwen-oauth" {
                    Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string())
                } else if provider == "google-gemini-cli" {
                    Some("cloudcode-pa://google".to_string())
                } else if provider == "stepfun" {
                    Some("https://api.stepfun.ai/step_plan/v1".to_string())
                } else {
                    None
                }
            })
    }

    fn resolve_oauth_store_api_key(&self, provider: &str) -> Option<String> {
        let provider_key = match provider {
            "openai-codex" | "codex" => "openai-codex",
            "qwen-oauth" => "qwen-oauth",
            "anthropic" | "claude" | "claude-code" => "anthropic",
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => "google-gemini-cli",
            _ => return None,
        };
        let path = self.auth_tokens_path();
        let raw = std::fs::read_to_string(path).ok()?;
        let entries: HashMap<String, OAuthStoreCredential> = serde_json::from_str(&raw).ok()?;
        let cred = entries.get(provider_key)?;
        if cred.access_token.trim().is_empty() {
            return None;
        }
        if cred
            .expires_at
            .map(|exp| exp <= Utc::now())
            .unwrap_or(false)
        {
            return None;
        }
        Some(cred.access_token.clone())
    }

    async fn refresh_oauth_store_tokens_if_needed(&self) {
        // Keep this list explicit so behavior is deterministic and parity-scoped.
        self.refresh_single_oauth_store_token_if_needed("openai-codex")
            .await;
        self.refresh_single_oauth_store_token_if_needed("qwen-oauth")
            .await;
        self.refresh_single_oauth_store_token_if_needed("anthropic")
            .await;
    }

    async fn refresh_single_oauth_store_token_if_needed(&self, provider_key: &str) {
        if !self.can_attempt_oauth_refresh(provider_key) {
            return;
        }
        let path = self.auth_tokens_path();
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut entries: HashMap<String, OAuthStoreCredential> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(current) = entries.get(provider_key).cloned() else {
            return;
        };
        let Some(expires_at) = current.expires_at else {
            return;
        };
        if expires_at > Utc::now() {
            return;
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            return;
        };
        let Some((token_url, client_id)) = self.oauth_refresh_config(provider_key) else {
            return;
        };
        let refreshed = match self
            .exchange_oauth_refresh_token(
                provider_key,
                token_url.as_str(),
                client_id.as_str(),
                refresh_token.as_str(),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.mark_oauth_refresh_failure(provider_key);
                tracing::warn!(
                    provider = provider_key,
                    error = %e,
                    "oauth token refresh failed for runtime provider"
                );
                return;
            }
        };
        entries.insert(provider_key.to_string(), refreshed);
        let Ok(content) = serde_json::to_string_pretty(&entries) else {
            self.mark_oauth_refresh_success(provider_key);
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(path, content).await;
        self.mark_oauth_refresh_success(provider_key);
    }

    fn oauth_refresh_config(&self, provider_key: &str) -> Option<(String, String)> {
        // Preferred source: unified provider config centre (runtime_providers).
        let cfg_token_url = self
            .config
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_token_url.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let cfg_client_id = self
            .config
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_client_id.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Env fallback — keeps previous behavior working when config centre is empty.
        let (token_url_env, client_id_env) = match provider_key {
            "openai-codex" => (
                "HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL",
                "HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID",
            ),
            "qwen-oauth" => ("HERMES_QWEN_OAUTH_TOKEN_URL", "HERMES_QWEN_OAUTH_CLIENT_ID"),
            "anthropic" => (
                "HERMES_ANTHROPIC_OAUTH_TOKEN_URL",
                "HERMES_ANTHROPIC_OAUTH_CLIENT_ID",
            ),
            _ => return cfg_token_url.zip(cfg_client_id),
        };
        let token_url = cfg_token_url
            .or_else(|| {
                std::env::var(token_url_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                if provider_key == "anthropic" {
                    Some("https://console.anthropic.com/v1/oauth/token".to_string())
                } else {
                    None
                }
            })?;
        let client_id = cfg_client_id
            .or_else(|| {
                std::env::var(client_id_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                if provider_key == "anthropic" {
                    Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string())
                } else {
                    None
                }
            })?;
        Some((token_url, client_id))
    }

    async fn exchange_oauth_refresh_token(
        &self,
        provider_key: &str,
        token_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<OAuthStoreCredential, AgentError> {
        let endpoints = OAuth2Endpoints {
            authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
            token_url: token_url.to_string(),
            client_id: client_id.to_string(),
            redirect_uri: "http://127.0.0.1/unused".to_string(),
            scopes: vec![],
        };
        let cred = exchange_refresh_token(provider_key, &endpoints, refresh_token)
            .await
            .map_err(|e| AgentError::AuthFailed(e.to_string()))?;
        Ok(OAuthStoreCredential {
            provider: Some(provider_key.to_string()),
            access_token: cred.access_token,
            refresh_token: cred
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            token_type: Some(cred.token_type),
            scope: cred.scope,
            expires_at: cred.expires_at,
        })
    }

    fn can_attempt_oauth_refresh(&self, provider_key: &str) -> bool {
        let Ok(guard) = self.oauth_refresh_backoff.lock() else {
            return true;
        };
        let Some(last_fail) = guard.get(provider_key) else {
            return true;
        };
        last_fail.elapsed().as_secs() >= OAUTH_REFRESH_BACKOFF_SECS
    }

    fn mark_oauth_refresh_failure(&self, provider_key: &str) {
        if let Ok(mut guard) = self.oauth_refresh_backoff.lock() {
            guard.insert(provider_key.to_string(), Instant::now());
        }
    }

    fn mark_oauth_refresh_success(&self, provider_key: &str) {
        if let Ok(mut guard) = self.oauth_refresh_backoff.lock() {
            guard.remove(provider_key);
        }
    }

    fn auth_tokens_path(&self) -> PathBuf {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("auth").join("tokens.json")
    }

    fn resolve_runtime_command_args(
        &self,
        provider: Option<&str>,
    ) -> (Option<String>, Vec<String>) {
        let mut command = self
            .config
            .acp_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut args: Vec<String> = self
            .config
            .acp_args
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();

        if let Some(provider) = provider {
            if let Some(cfg) = self.config.runtime_providers.get(provider) {
                if let Some(cmd) = cfg
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    command = Some(cmd.to_string());
                }
                if !cfg.args.is_empty() {
                    args = cfg
                        .args
                        .iter()
                        .map(|a| a.trim().to_string())
                        .filter(|a| !a.is_empty())
                        .collect();
                }
            }
            if provider == "copilot-acp" {
                if command.is_none() {
                    command = std::env::var("HERMES_COPILOT_ACP_COMMAND")
                        .ok()
                        .or_else(|| std::env::var("COPILOT_CLI_PATH").ok())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("copilot".to_string()));
                }
                if args.is_empty() {
                    args = std::env::var("HERMES_COPILOT_ACP_ARGS")
                        .ok()
                        .and_then(|raw| shlex::split(raw.trim()))
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| vec!["--acp".to_string(), "--stdio".to_string()]);
                }
                if let Some(cmd) = command.as_deref() {
                    if let Ok(resolved) = which::which(cmd) {
                        command = Some(resolved.to_string_lossy().to_string());
                    }
                }
            }
        }
        (command, args)
    }

    fn build_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
        api_mode: Option<&ApiMode>,
        credential_pool: Option<&Arc<CredentialPool>>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        let api_key = self
            .resolve_runtime_api_key(provider, api_key_env_override, explicit_api_key)
            .ok_or_else(|| {
                AgentError::Config(format!(
                    "No API key configured for runtime-routed provider '{}'",
                    provider
                ))
            })?;
        let base_url = self.resolve_runtime_base_url(provider, route_base_url);
        let mode = api_mode.unwrap_or(&self.config.api_mode);

        let provider_obj: Arc<dyn LlmProvider> = match provider {
            "openai" | "codex" | "openai-codex" => {
                if matches!(mode, ApiMode::CodexResponses) {
                    let mut p = CodexProvider::new(&api_key).with_model(model_name);
                    if let Some(ref url) = base_url {
                        p = p.with_base_url(url.clone());
                    }
                    if let Some(pool) = credential_pool {
                        p = p.with_credential_pool(pool.clone());
                    }
                    Arc::new(p)
                } else {
                    let mut p = OpenAiProvider::new(&api_key).with_model(model_name);
                    if let Some(url) = base_url {
                        p = p.with_base_url(url);
                    }
                    if let Some(pool) = credential_pool {
                        p = p.with_credential_pool(pool.clone());
                    }
                    Arc::new(p)
                }
            }
            "anthropic" => {
                let mut p = AnthropicProvider::new(&api_key).with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            }
            "openrouter" => {
                let mut p = OpenRouterProvider::new(&api_key).with_model(model_name);
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            }
            "qwen" | "qwen-oauth" => {
                let mut p = QwenProvider::new(&api_key).with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "kimi" | "moonshot" => {
                let mut p = KimiProvider::new(&api_key).with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "minimax" => {
                let mut p = MiniMaxProvider::new(&api_key).with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "stepfun" => {
                let url =
                    base_url.unwrap_or_else(|| "https://api.stepfun.ai/step_plan/v1".to_string());
                Arc::new(GenericProvider::new(url, &api_key, model_name))
            }
            "nous" => {
                let mut p = NousProvider::new(&api_key).with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "copilot" | "copilot-acp" => {
                let p = CopilotProvider::new(
                    base_url.unwrap_or_else(|| "https://api.github.com/copilot".to_string()),
                    &api_key,
                )
                .with_model(model_name);
                Arc::new(p)
            }
            _ => {
                let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let mut g = GenericProvider::new(url, &api_key, model_name);
                if let Some(pool) = credential_pool {
                    g = g.with_credential_pool(pool.clone());
                }
                Arc::new(g)
            }
        };
        Ok(provider_obj)
    }

    fn credential_pool_for_route<'a>(
        &'a self,
        rt: &'a TurnRuntimeRoute,
    ) -> Option<&'a Arc<CredentialPool>> {
        if rt.credential_pool_fallback {
            rt.credential_pool
                .as_ref()
                .or(self.primary_credential_pool.as_ref())
        } else {
            rt.credential_pool.as_ref()
        }
    }

    fn messages_for_api_call(&self, ctx: &ContextManager) -> Vec<Message> {
        let mut messages = ctx.get_messages().to_vec();
        if let Some(ephemeral) = self
            .config
            .ephemeral_system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            messages.push(Message::system(ephemeral));
        }
        self.apply_prompt_cache_markers(&mut messages);
        messages
    }

    fn apply_prompt_cache_markers(&self, messages: &mut Vec<Message>) {
        use hermes_core::types::{CacheControl, CacheType, MessageRole};
        if messages.is_empty() {
            return;
        }
        let provider = self
            .config
            .provider
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_anthropic = provider.contains("anthropic") || provider.contains("claude");
        if !is_anthropic {
            return;
        }
        for msg in messages.iter_mut() {
            if msg.role == MessageRole::System {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Persistent,
                });
                break;
            }
        }
        let ephemeral_budget = 4;
        let mut marked = 0;
        for msg in messages.iter_mut().rev() {
            if marked >= ephemeral_budget {
                break;
            }
            if msg.role == MessageRole::User || msg.role == MessageRole::Assistant {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Ephemeral,
                });
                marked += 1;
            }
        }
    }

    /// Build the full system prompt including identity, memory, and plugin context.
    ///
    /// Aligns with Python behavior:
    /// - prefer `~/.hermes/SOUL.md` as identity
    /// - fallback to `DEFAULT_AGENT_IDENTITY`
    /// - then append optional configured `system_prompt`
    fn build_system_prompt(
        &self,
        _task_hint: &str,
        tool_schemas: &[ToolSchema],
        model_for_prompt: &str,
    ) -> String {
        let soul = load_soul_md();
        let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
        if let Some(base) = self.config.system_prompt.as_deref() {
            builder = builder.with_system_message(base);
        }
        let tool_names: HashSet<&str> = tool_schemas.iter().map(|t| t.name.as_str()).collect();
        let mut tool_guidance = Vec::new();
        if tool_names.contains("memory") {
            tool_guidance.push(MEMORY_GUIDANCE);
        }
        if tool_names.contains("session_search") {
            tool_guidance.push(SESSION_SEARCH_GUIDANCE);
        }
        if tool_names.contains("skill_manage") {
            tool_guidance.push(SKILLS_GUIDANCE);
        }
        if !tool_guidance.is_empty() {
            builder = builder.with_tool_guidance(&tool_guidance.join(" "));
        }

        if !tool_names.is_empty() && self.should_inject_tool_enforcement(model_for_prompt) {
            builder = builder.with_block(TOOL_USE_ENFORCEMENT_GUIDANCE);
            let model_lower = model_for_prompt.to_lowercase();
            if model_lower.contains("gemini") || model_lower.contains("gemma") {
                builder = builder.with_block(GOOGLE_MODEL_OPERATIONAL_GUIDANCE);
            }
            if model_lower.contains("gpt") || model_lower.contains("codex") {
                builder = builder.with_block(OPENAI_MODEL_EXECUTION_GUIDANCE);
            }
        }

        if let Some(ref personality) = self.config.personality {
            let requested = personality.trim();
            if !requested.is_empty() {
                if let Some(profile) =
                    resolve_personality(requested, self.config.hermes_home.as_deref())
                {
                    builder = builder
                        .with_block(&format!("## Active Personality ({requested})\n{profile}"));
                } else if requested.contains(char::is_whitespace) {
                    // Compatibility path: historically this field was appended verbatim.
                    builder = builder.with_block(&format!("Personality: {requested}"));
                    tracing::warn!(
                        "personality '{requested}' not found as a named profile; using inline value"
                    );
                } else {
                    tracing::warn!(
                        "personality '{}' not found; falling back to default identity",
                        requested
                    );
                }
            }
        }

        if !self.config.skip_memory {
            let (memory_block, user_block) =
                load_builtin_memory_snapshot(self.config.hermes_home.as_deref());
            if let Some(block) = memory_block {
                builder = builder.with_block(&block);
            }
            if let Some(block) = user_block {
                builder = builder.with_block(&block);
            }
        }

        let mem_block = self.memory_system_prompt();
        if !mem_block.is_empty() {
            builder = builder.with_memory_context(&mem_block);
        }

        if let Some(skills_prompt) = self.skills_system_prompt(&tool_names) {
            builder = builder.with_skills_prompt(&skills_prompt);
        }

        if let Some(context_prompt) = self.context_files_prompt() {
            builder = builder.with_context_files(&context_prompt);
        }
        if let Some(repo_map) = self.code_index_repo_map_block() {
            builder = builder.with_block(&repo_map);
        }

        let provider = self.effective_provider_for_prompt(model_for_prompt);
        builder = builder.with_timestamp(Some(model_for_prompt), provider.as_deref());

        let mut timestamp_extras = String::new();
        if self.config.pass_session_id {
            if let Some(ref sid) = self.config.session_id {
                if !sid.trim().is_empty() {
                    timestamp_extras.push_str(&format!("Session ID: {}\n", sid.trim()));
                }
            }
        }
        if !timestamp_extras.trim().is_empty() {
            builder = builder.with_block(timestamp_extras.trim_end());
        }

        if provider.as_deref() == Some("alibaba") {
            let model_short = model_for_prompt
                .split('/')
                .next_back()
                .unwrap_or(model_for_prompt);
            builder = builder.with_block(&format!(
                "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
                model_short, model_for_prompt
            ));
        }

        if let Some(hint) = self.platform_hint_text() {
            builder = builder.with_block(hint);
        }

        builder.build().to_string()
    }

    /// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    fn resolve_initial_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        if let Some(ref s) = self.config.stored_system_prompt {
            let t = s.trim();
            if !t.is_empty() {
                return (s.clone(), true);
            }
        }
        (
            self.build_system_prompt(task_hint, tool_schemas, &self.config.model),
            false,
        )
    }

    /// Compress when total chars exceed 80% of the context budget (Python auto-compaction).
    fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        let threshold = (max_c as f64 * 0.8) as usize;
        if total_chars <= threshold {
            return;
        }
        let message = format!(
            "Context pressure at {}%, triggering compression",
            (total_chars * 100) / max_c
        );
        tracing::info!("{message}");
        self.emit_status("lifecycle", &message);
        if let Some(note) = self.memory_pre_compress_note(ctx.get_messages()) {
            ctx.add_message(Message::system(note));
        }
        ctx.compress();
    }

    fn emit_status(&self, event_type: &str, message: &str) {
        if self.config.quiet_mode {
            return;
        }
        if let Some(cb) = self.callbacks.status_callback.as_ref() {
            cb(event_type, message);
        }
    }

    fn should_emit_context_pressure_warning(
        progress_ratio: f64,
        tier: f64,
        warned_tier: &mut f64,
        last_warn_at: &mut Option<Instant>,
        last_warn_percent: &mut f64,
    ) -> bool {
        if tier <= 0.0 {
            return false;
        }
        let progress_percent = progress_ratio * 100.0;
        let now = Instant::now();
        const WARN_COOLDOWN_SECS: u64 = 20;
        const WARN_PERCENT_STEP: f64 = 5.0;

        let tier_upgraded = tier > *warned_tier;
        let cooldown_elapsed = last_warn_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(WARN_COOLDOWN_SECS))
            .unwrap_or(true);
        let percent_advanced = (progress_percent - *last_warn_percent) >= WARN_PERCENT_STEP;

        if tier_upgraded || (cooldown_elapsed && percent_advanced) {
            if tier_upgraded {
                *warned_tier = tier;
            }
            *last_warn_at = Some(now);
            *last_warn_percent = progress_percent;
            return true;
        }
        false
    }

    fn assistant_visible_text(m: &Message) -> bool {
        m.content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        let Some(content) = m.content.as_deref() else {
            return false;
        };
        !strip_think_blocks_for_ack(content).trim().is_empty()
    }

    fn assistant_has_reasoning(m: &Message) -> bool {
        m.reasoning_content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
        let trimmed = tc.function.arguments.trim();
        if trimmed.is_empty() {
            tc.function.arguments = "{}".to_string();
            return Ok(());
        }
        serde_json::from_str::<Value>(trimmed)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn extra_body_for_api_mode(&self, api_mode: &ApiMode) -> Option<Value> {
        let mut body = self
            .config
            .extra_body
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        if !body.is_object() {
            return self.config.extra_body.clone();
        }
        if !matches!(api_mode, ApiMode::CodexResponses) {
            if body.get("strict_tool_calls").is_none()
                && body.get("strict_api").is_none()
                && body.get("provider_strict").is_none()
            {
                body["strict_api"] = Value::Bool(true);
            }
        }
        Some(body)
    }

    // -- Retry-aware LLM call ---------------------------------------------

    fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermes_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, route, max_tokens_override))
    }

    async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        let model = route
            .map(|r| r.model.as_str())
            .unwrap_or(self.config.model.as_str());
        let route_provider_hint = route
            .and_then(|r| r.provider.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let inferred_provider = self.extract_provider_and_model(model).0;
        let active_provider = route_provider_hint.unwrap_or(inferred_provider);
        if let Some(rt) = route {
            if let Some(ref label) = rt.route_label {
                tracing::debug!(%label, model = %rt.model, ?rt.signature, "smart model route");
            }
            if rt.command.is_some() || !rt.args.is_empty() {
                tracing::debug!(command = ?rt.command, args = ?rt.args, "smart route process metadata");
            }
        }
        let api_messages = self.messages_for_api_call(ctx);
        let retry = &self.config.retry;
        let (effective_max_retries, effective_base_delay_ms) =
            (retry.max_retries, retry.base_delay_ms);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);
        let effective_max_tokens = max_tokens_override.or(self.config.max_tokens);

        for attempt in 0..=effective_max_retries {
            self.interrupt.check_interrupt()?;
            let result = if let Some(rt) = route {
                let (provider_name, model_name) = self.extract_provider_and_model(model);
                let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                let extra_body_for_call = self.extra_body_for_api_mode(mode);
                let pool = self.credential_pool_for_route(rt);
                let routed_provider = self.build_runtime_provider(
                    rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                    model_name,
                    rt.base_url.as_deref(),
                    rt.api_key_env.as_deref(),
                    None,
                    Some(mode),
                    pool,
                );
                match routed_provider {
                    Ok(provider) => {
                        provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                effective_max_tokens,
                                self.config.temperature,
                                Some(model),
                                extra_body_for_call.as_ref(),
                            )
                            .await
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Runtime route unavailable (reason={:?}), falling back to primary runtime: {}",
                            rt.routing_reason,
                            e
                        );
                        self.llm_provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                effective_max_tokens,
                                self.config.temperature,
                                Some(self.config.model.as_str()),
                                default_extra_body.as_ref(),
                            )
                            .await
                    }
                }
            } else {
                self.llm_provider
                    .chat_completion(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        self.config.temperature,
                        Some(model),
                        default_extra_body.as_ref(),
                    )
                    .await
            };

            match result {
                Ok(response) => return Ok(response),
                Err(e) => {
                    let err_str = e.to_string();
                    let class = classify_error(&err_str);
                    tracing::warn!(
                        attempt,
                        error_class = ?class,
                        "LLM API error: {}",
                        &err_str[..err_str.len().min(200)]
                    );

                    match class {
                        ErrorClass::Auth => {
                            if let Some(diag) = maybe_nous_401_diagnostic(
                                active_provider.as_str(),
                                &err_str,
                                self.config.hermes_home.as_deref(),
                            ) {
                                self.emit_status("lifecycle", &diag);
                                return Err(AgentError::LlmApi(format!("{err_str}\n\n{diag}")));
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::Fatal => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::ContextOverflow => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::RateLimit | ErrorClass::Retryable => {
                            if attempt >= effective_max_retries {
                                if let Some(ref fallback) = retry.fallback_model {
                                    if model != fallback.as_str() {
                                        tracing::info!(
                                            "All retries exhausted on {}. Trying fallback: {}",
                                            model,
                                            fallback
                                        );
                                        let fallback_result = self
                                            .llm_provider
                                            .chat_completion(
                                                &api_messages,
                                                tool_schemas,
                                                effective_max_tokens,
                                                self.config.temperature,
                                                Some(fallback),
                                                default_extra_body.as_ref(),
                                            )
                                            .await;
                                        return fallback_result
                                            .map_err(|e| AgentError::LlmApi(e.to_string()));
                                    }
                                }
                                return Err(AgentError::LlmApi(err_str));
                            }
                            let delay = jittered_backoff(
                                attempt,
                                effective_base_delay_ms,
                                retry.max_delay_ms,
                            );
                            tracing::info!(
                                "Retrying in {}ms (attempt {}/{})",
                                delay.as_millis(),
                                attempt + 1,
                                effective_max_retries
                            );
                            self.emit_status(
                                "lifecycle",
                                &format!(
                                    "LLM API retry in {}ms (attempt {}/{})",
                                    delay.as_millis(),
                                    attempt + 1,
                                    effective_max_retries
                                ),
                            );
                            sleep(delay).await;
                        }
                    }
                }
            }
        }
        unreachable!()
    }

    fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
            let mut m = Message::assistant(content.to_string());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        } else {
            let content_opt = if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            };
            let mut m = Message::assistant_with_tool_calls(content_opt, tool_calls.to_vec());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        }
    }

    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
    async fn collect_stream_llm_response(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        active_model: &str,
        max_tokens_override: Option<u32>,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<StreamCollectOutcome, AgentError> {
        let api_messages = self.messages_for_api_call(ctx);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);
        let effective_max_tokens = max_tokens_override.or(self.config.max_tokens);
        let max_stream_retries = std::env::var("HERMES_STREAM_RETRIES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v.min(10))
            .unwrap_or(self.config.stream_read_max_retries.min(10));

        'stream_attempt: for stream_attempt in 0..=max_stream_retries {
            let mut stream = if let Some(rt) = route {
                let (provider_name, model_name) = self.extract_provider_and_model(active_model);
                let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                let extra_body_for_call = self.extra_body_for_api_mode(mode);
                let pool = self.credential_pool_for_route(rt);
                match self.build_runtime_provider(
                    rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                    model_name,
                    rt.base_url.as_deref(),
                    rt.api_key_env.as_deref(),
                    None,
                    Some(mode),
                    pool,
                ) {
                    Ok(provider) => provider.chat_completion_stream(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        self.config.temperature,
                        Some(active_model),
                        extra_body_for_call.as_ref(),
                    ),
                    Err(e) => {
                        tracing::warn!(
                            "Runtime route unavailable (reason={:?}) for stream, falling back to primary runtime: {}",
                            rt.routing_reason,
                            e
                        );
                        self.llm_provider.chat_completion_stream(
                            &api_messages,
                            tool_schemas,
                            effective_max_tokens,
                            self.config.temperature,
                            Some(self.config.model.as_str()),
                            default_extra_body.as_ref(),
                        )
                    }
                }
            } else {
                self.llm_provider.chat_completion_stream(
                    &api_messages,
                    tool_schemas,
                    effective_max_tokens,
                    self.config.temperature,
                    Some(active_model),
                    default_extra_body.as_ref(),
                )
            };

            let mut content = String::new();
            let mut reasoning_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut last_usage: Option<UsageStats> = None;
            let mut finish_reason: Option<String> = None;
            let mut deltas_were_sent = false;

            while let Some(chunk_result) = stream.next().await {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    let message = Self::assemble_stream_assistant_message(
                        &content,
                        &reasoning_content,
                        &tool_calls,
                    );
                    return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                        message,
                        usage: last_usage.clone(),
                        model: active_model.to_string(),
                        finish_reason: Some("interrupted".to_string()),
                    }));
                }

                let chunk = match chunk_result {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        let partial_tool_in_flight = tool_calls.iter().any(|tc| {
                            !tc.id.is_empty()
                                || !tc.function.name.is_empty()
                                || !tc.function.arguments.trim().is_empty()
                        });
                        let should_retry_for_partial_tool = deltas_were_sent
                            && partial_tool_in_flight
                            && is_transient_stream_error(&err)
                            && stream_attempt < max_stream_retries;
                        let should_retry_before_deltas = !deltas_were_sent
                            && is_transient_stream_error(&err)
                            && stream_attempt < max_stream_retries;

                        if should_retry_for_partial_tool || should_retry_before_deltas {
                            let next_attempt = stream_attempt + 2;
                            let total_attempts = max_stream_retries + 1;
                            if should_retry_for_partial_tool {
                                on_chunk(StreamChunk {
                                    delta: Some(hermes_core::StreamDelta {
                                        content: Some(
                                            "\n\n[connection dropped mid tool-call; reconnecting...]\n\n"
                                                .to_string(),
                                        ),
                                        tool_calls: None,
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                                self.emit_status(
                                    "lifecycle",
                                    &format!(
                                        "Connection dropped mid tool-call; reconnecting (attempt {}/{})",
                                        next_attempt, total_attempts
                                    ),
                                );
                                tracing::warn!(
                                    "Streaming attempt {}/{} failed after partial tool-call data; retrying: {}",
                                    stream_attempt + 1,
                                    total_attempts,
                                    err
                                );
                            } else {
                                tracing::warn!(
                                    "Streaming attempt {}/{} failed before deltas; retrying: {}",
                                    stream_attempt + 1,
                                    total_attempts,
                                    err
                                );
                            }
                            continue 'stream_attempt;
                        }
                        return Err(err);
                    }
                };

                if let Some(ref delta) = chunk.delta {
                    if let Some(ref text) = delta.content {
                        deltas_were_sent = true;
                        content.push_str(text);
                        if let Some(ref cb) = self.callbacks.on_stream_delta {
                            cb(text);
                        }
                    }
                    if let Some(ref extra) = delta.extra {
                        if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                            reasoning_content.push_str(thinking);
                            if let Some(ref cb) = self.callbacks.on_thinking {
                                cb(thinking);
                            }
                        }
                    }
                    if let Some(ref tc_deltas) = delta.tool_calls {
                        for tcd in tc_deltas {
                            let idx = tcd.index as usize;
                            while tool_calls.len() <= idx {
                                tool_calls.push(ToolCall {
                                    id: String::new(),
                                    function: hermes_core::FunctionCall {
                                        name: String::new(),
                                        arguments: String::new(),
                                    },
                                    extra_content: None,
                                });
                            }
                            if let Some(ref id) = tcd.id {
                                tool_calls[idx].id = id.clone();
                            }
                            if let Some(ref fc) = tcd.function {
                                if let Some(ref name) = fc.name {
                                    tool_calls[idx].function.name = name.clone();
                                }
                                if let Some(ref args) = fc.arguments {
                                    tool_calls[idx].function.arguments.push_str(args);
                                }
                            }
                        }
                    }
                }

                if let Some(ref usage) = chunk.usage {
                    last_usage = Some(usage.clone());
                }
                if let Some(ref fr) = chunk.finish_reason {
                    finish_reason = Some(fr.clone());
                }

                on_chunk(chunk);
            }

            let has_truncated_tool_args = tool_calls.iter().any(|tc| {
                let trimmed = tc.function.arguments.trim();
                !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_err()
            });
            if has_truncated_tool_args {
                finish_reason = Some("length".to_string());
            }

            let message =
                Self::assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);

            return Ok(StreamCollectOutcome::Complete(LlmResponse {
                message,
                usage: last_usage,
                model: active_model.to_string(),
                finish_reason,
            }));
        }

        Err(AgentError::LlmApi(
            "streaming failed after retry budget exhausted".to_string(),
        ))
    }

    /// Run the agent loop (non-streaming).
    ///
    /// Sends the initial messages to the LLM, then iteratively:
    /// - Executes any tool calls the LLM makes
    /// - Feeds results back as tool messages
    /// - Stops when the LLM responds without tool calls, or max turns exceeded
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut messages = messages;
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let context_length = get_model_context_length(&self.config.model);
        for msg in messages.iter_mut() {
            if msg.role != MessageRole::User {
                continue;
            }
            let Some(content) = msg.content.clone() else {
                continue;
            };
            let result =
                preprocess_context_references_async(&content, &cwd, context_length, None).await;
            if result.expanded && result.message != content {
                msg.content = Some(result.message);
            }
        }

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Determine which tools to expose
        let tool_schemas: Vec<ToolSchema> = tools.unwrap_or_else(|| self.tool_registry.schemas());

        // Build and inject system prompt (or reuse SQLite-cached prompt for session continuity)
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == MessageRole::User)
                .last()
                .map(|(i, _)| i)
        } else {
            None
        };
        let mut codex_ack_continuations: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch for first user message
        let first_user = ctx
            .get_messages()
            .iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id);
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        if self.config.preflight_context_compress {
            self.auto_compress_if_over_threshold(&mut ctx);
        }
        let replay = ReplayRecorder::for_session(&self.config, session_id);
        replay.record(
            "session_start",
            serde_json::json!({
                "session_id": session_id,
                "mode": "run",
                "model": self.config.model,
                "max_turns": self.config.max_turns,
            }),
        );

        let mut total_turns: u32 = 0;
        let mut _total_api_time_ms: u64 = 0;
        let mut _total_tool_time_ms: u64 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;
        let mut session_cost_usd: f64 = 0.0;
        let mut cost_warned = false;
        let mut forced_runtime_route: Option<TurnRuntimeRoute> = None;
        let mut last_checkpoint_messages: Option<Vec<Message>> = None;
        let mut invalid_tool_retries: u32 = 0;
        let mut invalid_json_retries: u32 = 0;
        let mut last_content_with_tools: Option<String> = None;
        let mut context_pressure_warned_at: f64 = 0.0;
        let mut context_pressure_last_warn_at: Option<Instant> = None;
        let mut context_pressure_last_warn_percent: f64 = 0.0;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                self.memory_on_session_end(ctx.get_messages());
                replay.record(
                    "session_end",
                    serde_json::json!({
                        "reason": "max_turns",
                        "total_turns": total_turns,
                        "session_cost_usd": session_cost_usd,
                    }),
                );
                return Ok(AgentResult {
                    messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                    finished_naturally: false,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                    interrupted: false,
                    session_cost_usd: Some(session_cost_usd),
                    session_started_hooks_fired,
                });
            }

            total_turns += 1;
            tracing::debug!("Agent turn {}", total_turns);

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.refresh_oauth_store_tokens_if_needed().await;

            // Skill nudge counter — Python `run_agent.py`: increment at the start of each inner API iteration.
            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config.checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            // Notify memory + plugins of new turn
            self.memory_on_turn_start(total_turns, "");

            // Memory sync at flush interval
            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id);
            }

            // --- Pre-LLM hook ---
            let turn_runtime_route = forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let llm_governor = governor_for_turn(&self.config, &ctx, 0);
            tracing::debug!(
                turn = total_turns,
                model = active_model,
                governor_pressure = llm_governor.pressure,
                governor_max_tokens = ?llm_governor.max_tokens,
                "turn governor snapshot"
            );
            replay.record(
                "turn_start",
                serde_json::json!({
                    "turn": total_turns,
                    "model": active_model,
                    "pressure": llm_governor.pressure,
                    "max_tokens": llm_governor.max_tokens,
                }),
            );
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- LLM API call with transport retry + semantic empty/thinking recovery (Python parity) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        accumulated_usage.clone(),
                        session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = match self
                    .call_llm_with_retry(
                        &ctx,
                        &tool_schemas,
                        turn_runtime_route.as_ref(),
                        llm_governor.max_tokens,
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(AgentError::Interrupted { .. }) => {
                        return Ok(self.graceful_interrupt_result(
                            &ctx,
                            total_turns,
                            &tool_errors,
                            accumulated_usage.clone(),
                            session_cost_usd,
                            session_started_hooks_fired,
                            persist_user_idx,
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                let has_tools = r
                    .message
                    .tool_calls
                    .as_ref()
                    .map_or(false, |tc| !tc.is_empty());
                if has_tools {
                    break r;
                }
                if Self::assistant_visible_text(&r.message) {
                    break r;
                }
                if Self::assistant_has_reasoning(&r.message)
                    && inner_thinking < self.config.thinking_prefill_max_retries
                {
                    inner_thinking += 1;
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Reasoning-only response — retrying ({}/{})",
                            inner_thinking, self.config.thinking_prefill_max_retries
                        ),
                    );
                    ctx.add_message(r.message.clone());
                    continue;
                }
                // Accept explicit stop/end-turn responses even when assistant text is empty.
                // Anthropic can return this shape after trivial tool side-effects.
                if !Self::assistant_has_reasoning(&r.message)
                    && r.finish_reason.as_deref() == Some("stop")
                {
                    break r;
                }
                if !Self::assistant_has_reasoning(&r.message)
                    && inner_empty < self.config.empty_content_max_retries
                {
                    inner_empty += 1;
                    tracing::warn!(
                        "empty assistant response — retrying ({}/{})",
                        inner_empty,
                        self.config.empty_content_max_retries
                    );
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Empty assistant response — retrying ({}/{})",
                            inner_empty, self.config.empty_content_max_retries
                        ),
                    );
                    continue;
                }
                break r;
            };
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            _total_api_time_ms += api_elapsed;
            replay.record(
                "llm_response",
                serde_json::json!({
                    "turn": total_turns,
                    "model": response.model,
                    "finish_reason": response.finish_reason,
                    "api_time_ms": api_elapsed,
                    "tool_call_count": response.message.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0),
                    "has_visible_text": Self::assistant_visible_text(&response.message),
                }),
            );

            // --- Post-LLM hook ---
            let post_ctx = serde_json::json!({
                "turn": total_turns,
                "api_time_ms": api_elapsed,
                "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
            });
            let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
            self.inject_hook_context(&post_results, &mut ctx);

            // Accumulate usage (merged across semantic-retried sub-calls)
            if let Some(ref usage) = turn_usage_acc {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) =
                    estimate_usage_cost_usd(usage, response.model.as_str(), &self.config)
                {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config.max_cost_usd {
                if !cost_warned
                    && session_cost_usd >= limit * self.config.cost_guard_degrade_at_ratio
                {
                    cost_warned = true;
                    if forced_runtime_route.is_none() {
                        if let Some(model) = self.resolve_cost_degrade_model() {
                            forced_runtime_route = Some(self.turn_route_cost_guard(model.clone()));
                            ctx.add_message(Message::system(format!(
                                "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                                session_cost_usd, limit, model
                            )));
                        } else {
                            ctx.add_message(Message::system(format!(
                                "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                                session_cost_usd, limit
                            )));
                        }
                    }
                }
                if session_cost_usd >= limit {
                    ctx.add_message(Message::system(format!(
                        "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                        session_cost_usd, limit
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
            }

            let history_includes_tool = ctx
                .get_messages()
                .iter()
                .any(|m| m.role == MessageRole::Tool);
            let assistant_msg = response.message.clone();
            let tool_calls = assistant_msg.tool_calls.clone();
            ctx.add_message(assistant_msg.clone());
            if assistant_msg
                .tool_calls
                .as_ref()
                .map_or(false, |v| !v.is_empty())
                && Self::assistant_visible_text_after_think_blocks(&assistant_msg)
            {
                last_content_with_tools = assistant_msg
                    .content
                    .as_deref()
                    .map(strip_think_blocks_for_ack)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }

            // Step complete callback
            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done
            let tool_calls = match tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    if self.config.api_mode == ApiMode::CodexResponses
                        && !tool_schemas.is_empty()
                        && codex_ack_continuations < 2
                        && looks_like_codex_intermediate_ack(
                            &task_hint,
                            assistant_msg.content.as_deref().unwrap_or(""),
                            history_includes_tool,
                        )
                    {
                        codex_ack_continuations += 1;
                        ctx.add_message(Message::user(CODEX_CONTINUE_USER_MESSAGE));
                        continue;
                    }
                    if !Self::assistant_visible_text_after_think_blocks(&assistant_msg) {
                        if let Some(fallback) = last_content_with_tools.take() {
                            if let Some(last) = ctx.get_messages_mut().last_mut() {
                                if last.role == MessageRole::Assistant {
                                    last.content = Some(fallback);
                                }
                            }
                        }
                    }
                    tracing::debug!("No tool calls in response, finishing naturally");
                    // Final memory sync
                    let (u, a) = extract_last_user_assistant(ctx.get_messages());
                    self.memory_sync(&u, &a, session_id);
                    self.spawn_background_review(total_turns, &ctx, review_memory_at_end);
                    self.memory_on_session_end(ctx.get_messages());
                    replay.record(
                        "session_end",
                        serde_json::json!({
                            "reason": "finished_naturally",
                            "total_turns": total_turns,
                            "session_cost_usd": session_cost_usd,
                        }),
                    );
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                        finished_naturally: true,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
            };

            codex_ack_continuations = 0;

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                invalid_tool_retries = invalid_tool_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        invalid_tool_retries, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if invalid_tool_retries >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
                }
                continue;
            }
            invalid_tool_retries = 0;

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                invalid_json_retries = invalid_json_retries.saturating_add(1);
                if invalid_json_retries < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            invalid_json_retries, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                invalid_json_retries = 0;
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
                }
                continue;
            }
            invalid_json_retries = 0;

            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }

            // Cap concurrent delegate_task calls
            self.cap_delegates(&mut tool_calls);

            // --- Pre-tool hook ---
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }
            let tool_start = Instant::now();
            let tool_governor = governor_for_turn(&self.config, &ctx, tool_calls.len());
            let results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    tool_governor.tool_concurrency,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            _total_tool_time_ms += tool_elapsed;
            replay.record(
                "tool_batch",
                serde_json::json!({
                    "turn": total_turns,
                    "tool_count": tool_calls.len(),
                    "tool_concurrency": tool_governor.tool_concurrency,
                    "tool_time_ms": tool_elapsed,
                    "errors": results.iter().filter(|r| r.is_error).count(),
                }),
            );

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // --- Post-tool hook ---
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "is_error": res.is_error,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            // Enforce budget on tool results
            let mut results = results;
            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }
            let lsp_note = self.lsp_context_note(&tool_calls, &results);

            for result in results {
                replay.record(
                    "tool_result",
                    serde_json::json!({
                        "turn": total_turns,
                        "tool_call_id": result.tool_call_id,
                        "is_error": result.is_error,
                        "content_preview": result.content.chars().take(240).collect::<String>(),
                    }),
                );
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            if let Some(note) = lsp_note {
                ctx.add_message(Message::system(note));
            }
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_chars = ctx.total_chars();
            let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8) as usize;
            if threshold > 0 {
                let progress = total_chars as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if Self::should_emit_context_pressure_warning(
                    progress,
                    tier,
                    &mut context_pressure_warned_at,
                    &mut context_pressure_last_warn_at,
                    &mut context_pressure_last_warn_percent,
                ) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        total_chars,
                        threshold
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx);
        }
    }

    /// Run the agent loop with streaming.
    ///
    /// Uses the LLM provider's streaming API and invokes `on_chunk` for each
    /// incremental delta. The stream is collected into a complete response
    /// before tool execution proceeds.
    pub async fn run_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
        on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Result<AgentResult, AgentError> {
        let on_chunk = match on_chunk {
            Some(cb) => cb,
            None => {
                return self.run(messages, tools).await;
            }
        };
        let stream_mute = Arc::new(AtomicBool::new(false));
        let stream_needs_break = Arc::new(AtomicBool::new(false));
        let raw_emit = on_chunk;
        let mute_for_emit = stream_mute.clone();
        let break_for_emit = stream_needs_break.clone();
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> =
            Box::new(move |chunk: StreamChunk| {
                let StreamChunk {
                    delta,
                    finish_reason,
                    usage,
                } = chunk;
                if let Some(delta_val) = delta {
                    if let Some(content) = delta_val.content.clone() {
                        if mute_for_emit.load(Ordering::Acquire) {
                            return;
                        }
                        let mut out = content;
                        if break_for_emit.swap(false, Ordering::AcqRel) {
                            out = format!("\n\n{}", out);
                        }
                        raw_emit(StreamChunk {
                            delta: Some(hermes_core::StreamDelta {
                                content: Some(out),
                                tool_calls: delta_val.tool_calls,
                                extra: delta_val.extra,
                            }),
                            finish_reason,
                            usage,
                        });
                        return;
                    }
                    raw_emit(StreamChunk {
                        delta: Some(delta_val),
                        finish_reason,
                        usage,
                    });
                    return;
                }
                raw_emit(StreamChunk {
                    delta: None,
                    finish_reason,
                    usage,
                });
            });

        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut messages = messages;
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let tool_schemas: Vec<ToolSchema> = tools.unwrap_or_else(|| self.tool_registry.schemas());
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == MessageRole::User)
                .last()
                .map(|(i, _)| i)
        } else {
            None
        };
        let mut codex_ack_continuations: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch
        let first_user = ctx
            .get_messages()
            .iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id);
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        if self.config.preflight_context_compress {
            self.auto_compress_if_over_threshold(&mut ctx);
        }
        let replay = ReplayRecorder::for_session(&self.config, session_id);
        replay.record(
            "session_start",
            serde_json::json!({
                "session_id": session_id,
                "mode": "stream",
                "model": self.config.model,
                "max_turns": self.config.max_turns,
            }),
        );

        let mut total_turns: u32 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;
        let mut session_cost_usd: f64 = 0.0;
        let mut cost_warned = false;
        let mut forced_runtime_route: Option<TurnRuntimeRoute> = None;
        let mut last_checkpoint_messages: Option<Vec<Message>> = None;
        let mut invalid_tool_retries: u32 = 0;
        let mut invalid_json_retries: u32 = 0;
        let mut truncated_tool_call_retries: u32 = 0;
        let mut last_content_with_tools: Option<String> = None;
        let mut context_pressure_warned_at: f64 = 0.0;
        let mut context_pressure_last_warn_at: Option<Instant> = None;
        let mut context_pressure_last_warn_percent: f64 = 0.0;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                self.memory_on_session_end(ctx.get_messages());
                replay.record(
                    "session_end",
                    serde_json::json!({
                        "reason": "max_turns",
                        "total_turns": total_turns,
                        "session_cost_usd": session_cost_usd,
                    }),
                );
                return Ok(AgentResult {
                    messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                    finished_naturally: false,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                    interrupted: false,
                    session_cost_usd: Some(session_cost_usd),
                    session_started_hooks_fired,
                });
            }

            total_turns += 1;

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.refresh_oauth_store_tokens_if_needed().await;

            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            self.memory_on_turn_start(total_turns, "");

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config.checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
            }

            // Pre-LLM hook
            let turn_runtime_route = forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let llm_governor = governor_for_turn(&self.config, &ctx, 0);
            tracing::debug!(
                turn = total_turns,
                model = active_model,
                governor_pressure = llm_governor.pressure,
                governor_max_tokens = ?llm_governor.max_tokens,
                "turn governor snapshot"
            );
            replay.record(
                "turn_start",
                serde_json::json!({
                    "turn": total_turns,
                    "model": active_model,
                    "pressure": llm_governor.pressure,
                    "max_tokens": llm_governor.max_tokens,
                }),
            );
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Streaming first attempt + same D-step semantic recovery as `run()` (retries use non-stream) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let mut inner_attempt: u32 = 0;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        accumulated_usage.clone(),
                        session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = if inner_attempt == 0 {
                    match self
                        .collect_stream_llm_response(
                            &ctx,
                            &tool_schemas,
                            turn_runtime_route.as_ref(),
                            active_model,
                            llm_governor.max_tokens,
                            &*on_chunk,
                        )
                        .await?
                    {
                        StreamCollectOutcome::Complete(resp) => resp,
                        StreamCollectOutcome::Interrupted(partial) => {
                            if let Some(ref u) = partial.usage {
                                accumulated_usage = Some(merge_usage(accumulated_usage.clone(), u));
                                if let Some(cost) =
                                    estimate_usage_cost_usd(u, partial.model.as_str(), &self.config)
                                {
                                    session_cost_usd += cost;
                                }
                            }
                            ctx.add_message(partial.message);
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                accumulated_usage.clone(),
                                session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                    }
                } else {
                    match self
                        .call_llm_with_retry(
                            &ctx,
                            &tool_schemas,
                            turn_runtime_route.as_ref(),
                            llm_governor.max_tokens,
                        )
                        .await
                    {
                        Ok(r) => r,
                        Err(AgentError::Interrupted { .. }) => {
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                accumulated_usage.clone(),
                                session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                };
                inner_attempt = inner_attempt.saturating_add(1);

                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                let has_tools = r
                    .message
                    .tool_calls
                    .as_ref()
                    .map_or(false, |tc| !tc.is_empty());
                if has_tools {
                    break r;
                }
                if Self::assistant_visible_text(&r.message) {
                    break r;
                }
                if Self::assistant_has_reasoning(&r.message)
                    && inner_thinking < self.config.thinking_prefill_max_retries
                {
                    inner_thinking += 1;
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Reasoning-only response — retrying ({}/{})",
                            inner_thinking, self.config.thinking_prefill_max_retries
                        ),
                    );
                    ctx.add_message(r.message.clone());
                    continue;
                }
                // Accept explicit stop/end-turn responses even when assistant text is empty.
                // Anthropic can return this shape after trivial tool side-effects.
                if !Self::assistant_has_reasoning(&r.message)
                    && r.finish_reason.as_deref() == Some("stop")
                {
                    break r;
                }
                if !Self::assistant_has_reasoning(&r.message)
                    && inner_empty < self.config.empty_content_max_retries
                {
                    inner_empty += 1;
                    tracing::warn!(
                        "empty assistant response (stream path) — retrying ({}/{})",
                        inner_empty,
                        self.config.empty_content_max_retries
                    );
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Empty assistant response — retrying ({}/{})",
                            inner_empty, self.config.empty_content_max_retries
                        ),
                    );
                    continue;
                }
                break r;
            };
            let _api_elapsed_ms = api_start.elapsed().as_millis() as u64;
            replay.record(
                "llm_response",
                serde_json::json!({
                    "turn": total_turns,
                    "model": response.model,
                    "finish_reason": response.finish_reason,
                    "api_time_ms": _api_elapsed_ms,
                    "tool_call_count": response.message.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0),
                    "has_visible_text": Self::assistant_visible_text(&response.message),
                }),
            );

            // --- Post-LLM hook ---
            let post_ctx = serde_json::json!({
                "turn": total_turns,
                "api_time_ms": _api_elapsed_ms,
                "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
            });
            let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
            self.inject_hook_context(&post_results, &mut ctx);

            if let Some(ref usage) = turn_usage_acc {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) =
                    estimate_usage_cost_usd(usage, response.model.as_str(), &self.config)
                {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config.max_cost_usd {
                if !cost_warned
                    && session_cost_usd >= limit * self.config.cost_guard_degrade_at_ratio
                {
                    cost_warned = true;
                    if forced_runtime_route.is_none() {
                        if let Some(model) = self.resolve_cost_degrade_model() {
                            forced_runtime_route = Some(self.turn_route_cost_guard(model.clone()));
                            ctx.add_message(Message::system(format!(
                                "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                                session_cost_usd, limit, model
                            )));
                        } else {
                            ctx.add_message(Message::system(format!(
                                "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                                session_cost_usd, limit
                            )));
                        }
                    }
                }
                if session_cost_usd >= limit {
                    ctx.add_message(Message::system(format!(
                        "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                        session_cost_usd, limit
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    replay.record(
                        "session_end",
                        serde_json::json!({
                            "reason": "cost_guard",
                            "total_turns": total_turns,
                            "session_cost_usd": session_cost_usd,
                        }),
                    );
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
            }

            let history_includes_tool = ctx
                .get_messages()
                .iter()
                .any(|m| m.role == MessageRole::Tool);
            let assistant_msg = response.message.clone();
            let tool_calls = assistant_msg.tool_calls.clone();
            ctx.add_message(assistant_msg.clone());
            if assistant_msg
                .tool_calls
                .as_ref()
                .map_or(false, |v| !v.is_empty())
                && Self::assistant_visible_text_after_think_blocks(&assistant_msg)
            {
                last_content_with_tools = assistant_msg
                    .content
                    .as_deref()
                    .map(strip_think_blocks_for_ack)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
            if response.finish_reason.as_deref() == Some("length")
                && assistant_msg
                    .tool_calls
                    .as_ref()
                    .map_or(false, |calls| !calls.is_empty())
                && truncated_tool_call_retries < self.config.truncated_tool_call_max_retries
            {
                truncated_tool_call_retries = truncated_tool_call_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Truncated tool arguments — retrying ({}/{})",
                        truncated_tool_call_retries, self.config.truncated_tool_call_max_retries
                    ),
                );
                let _ = ctx.get_messages_mut().pop();
                continue;
            }
            truncated_tool_call_retries = 0;

            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            let tool_calls: Vec<ToolCall> = tool_calls
                .unwrap_or_default()
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                if self.config.api_mode == ApiMode::CodexResponses
                    && !tool_schemas.is_empty()
                    && codex_ack_continuations < 2
                    && looks_like_codex_intermediate_ack(
                        &task_hint,
                        assistant_msg.content.as_deref().unwrap_or(""),
                        history_includes_tool,
                    )
                {
                    codex_ack_continuations += 1;
                    ctx.add_message(Message::user(CODEX_CONTINUE_USER_MESSAGE));
                    continue;
                }
                if !Self::assistant_visible_text_after_think_blocks(&assistant_msg) {
                    if let Some(fallback) = last_content_with_tools.take() {
                        if let Some(last) = ctx.get_messages_mut().last_mut() {
                            if last.role == MessageRole::Assistant {
                                last.content = Some(fallback);
                            }
                        }
                    }
                }
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
                self.spawn_background_review(total_turns, &ctx, review_memory_at_end);
                self.memory_on_session_end(ctx.get_messages());
                replay.record(
                    "session_end",
                    serde_json::json!({
                        "reason": "finished_naturally",
                        "total_turns": total_turns,
                        "session_cost_usd": session_cost_usd,
                    }),
                );
                if stream_mute.swap(false, Ordering::AcqRel) {
                    on_chunk(StreamChunk {
                        delta: Some(hermes_core::StreamDelta {
                            content: None,
                            tool_calls: None,
                            extra: Some(serde_json::json!({
                                "control": "mute_post_response",
                                "enabled": false
                            })),
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                }
                return Ok(AgentResult {
                    messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                    finished_naturally: true,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                    interrupted: false,
                    session_cost_usd: Some(session_cost_usd),
                    session_started_hooks_fired,
                });
            }

            codex_ack_continuations = 0;

            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let all_housekeeping = tool_calls.iter().all(|tc| {
                matches!(
                    tc.function.name.as_str(),
                    "memory" | "todo" | "skill_manage" | "session_search"
                )
            });
            let should_mute_post =
                all_housekeeping && Self::assistant_visible_text_after_think_blocks(&assistant_msg);
            let was_muted = stream_mute.swap(should_mute_post, Ordering::AcqRel);
            if was_muted != should_mute_post {
                on_chunk(StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(serde_json::json!({
                            "control": "mute_post_response",
                            "enabled": should_mute_post
                        })),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }

            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                invalid_tool_retries = invalid_tool_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        invalid_tool_retries, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if invalid_tool_retries >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(&ctx, persist_user_idx),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
                }
                continue;
            }
            invalid_tool_retries = 0;

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                invalid_json_retries = invalid_json_retries.saturating_add(1);
                if invalid_json_retries < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            invalid_json_retries, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                invalid_json_retries = 0;
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
                }
                continue;
            }
            invalid_json_retries = 0;
            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }
            self.cap_delegates(&mut tool_calls);

            // Pre-tool hooks + callbacks
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            let mut results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    governor_for_turn(&self.config, &ctx, tool_calls.len()).tool_concurrency,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;
            replay.record(
                "tool_batch",
                serde_json::json!({
                    "turn": total_turns,
                    "tool_count": tool_calls.len(),
                    "tool_concurrency": governor_for_turn(&self.config, &ctx, tool_calls.len()).tool_concurrency,
                    "errors": results.iter().filter(|r| r.is_error).count(),
                }),
            );

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // Post-tool hooks + callbacks
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }
            let lsp_note = self.lsp_context_note(&tool_calls, &results);

            for result in results {
                replay.record(
                    "tool_result",
                    serde_json::json!({
                        "turn": total_turns,
                        "tool_call_id": result.tool_call_id,
                        "is_error": result.is_error,
                        "content_preview": result.content.chars().take(240).collect::<String>(),
                    }),
                );
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            if let Some(note) = lsp_note {
                ctx.add_message(Message::system(note));
            }
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            stream_needs_break.store(true, Ordering::Release);
            on_chunk(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({"control": "stream_break"})),
                }),
                finish_reason: None,
                usage: None,
            });
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_chars = ctx.total_chars();
            let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8) as usize;
            if threshold > 0 {
                let progress = total_chars as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if Self::should_emit_context_pressure_warning(
                    progress,
                    tier,
                    &mut context_pressure_warned_at,
                    &mut context_pressure_last_warn_at,
                    &mut context_pressure_last_warn_percent,
                ) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        total_chars,
                        threshold
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx);
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Remove duplicate tool calls that share the same function name and arguments.
    fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();
        for tc in calls {
            let key = format!("{}:{}", tc.function.name, tc.function.arguments);
            if seen.insert(key) {
                deduped.push(tc.clone());
            } else {
                tracing::warn!("Deduplicated tool call: {}", tc.function.name);
            }
        }
        deduped
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        if self.tool_registry.get(&tc.function.name).is_some() {
            return false;
        }
        let names = self.tool_registry.names();
        let target = tc.function.name.to_lowercase();

        if let Some(name) = names.iter().find(|n| n.to_lowercase() == target) {
            tracing::info!("Repaired tool call: '{}' → '{}'", tc.function.name, name);
            tc.function.name = name.clone();
            return true;
        }

        if let Some(name) = names
            .iter()
            .find(|n| n.to_lowercase().contains(&target) || target.contains(&n.to_lowercase()))
        {
            tracing::info!(
                "Repaired tool call (fuzzy): '{}' → '{}'",
                tc.function.name,
                name
            );
            tc.function.name = name.clone();
            return true;
        }
        false
    }

    /// Inject current session id into `session_search` calls when absent.
    fn hydrate_session_search_args(&self, tc: &mut ToolCall) {
        if tc.function.name != "session_search" {
            return;
        }
        let Some(session_id) = self.config.session_id.as_deref() else {
            return;
        };
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }

        let mut args: Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
        let Some(obj) = args.as_object_mut() else {
            return;
        };
        let has_current = obj
            .get("current_session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
        if has_current {
            return;
        }
        obj.insert(
            "current_session_id".to_string(),
            Value::String(session_id.to_string()),
        );
        if let Ok(updated) = serde_json::to_string(&args) {
            tc.function.arguments = updated;
        }
    }

    fn latest_user_text<'a>(&self, messages: &'a [Message]) -> Option<&'a str> {
        messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn primary_runtime_snapshot(&self) -> PrimaryRuntime {
        let provider = self
            .config
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let base_url = provider
            .as_ref()
            .and_then(|p| self.config.runtime_providers.get(p))
            .and_then(|c| {
                c.base_url
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
        let (command, args) = self.resolve_runtime_command_args(provider.as_deref());
        PrimaryRuntime {
            model: self.config.model.clone(),
            provider,
            base_url,
            api_mode: self.config.api_mode.clone(),
            command,
            args,
            credential_pool: self.primary_credential_pool.clone(),
        }
    }

    fn turn_route_cost_guard(&self, model: String) -> TurnRuntimeRoute {
        let pri = self.primary_runtime_snapshot();
        let mut sig = pri.to_signature();
        sig.model = model.clone();
        TurnRuntimeRoute {
            model,
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: self.primary_credential_pool.clone(),
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("cost_guard".to_string()),
            signature: sig,
        }
    }

    fn try_build_cheap_runtime(
        &self,
        cheap: &CheapModelRouteConfig,
        explicit_api_key: Option<String>,
    ) -> Result<ResolvedCheapRuntime, ()> {
        let provider_raw = cheap.provider.as_deref().map(str::trim).unwrap_or("");
        if provider_raw.is_empty() {
            return Err(());
        }
        let provider_lc = provider_raw.to_lowercase();
        let model_full = cheap.model.as_deref().map(str::trim).unwrap_or("");
        if model_full.is_empty() {
            return Err(());
        }
        let (_, model_name) = self.extract_provider_and_model(model_full);
        let base_url = self.resolve_runtime_base_url(&provider_lc, cheap.base_url.as_deref());
        let api_mode = base_url
            .as_deref()
            .and_then(detect_api_mode_for_url)
            .unwrap_or(ApiMode::ChatCompletions);

        let has_runtime_override = explicit_api_key
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .is_some()
            || cheap
                .base_url
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
        let pool_ref = if has_runtime_override {
            None
        } else {
            self.primary_credential_pool.as_ref()
        };

        self.build_runtime_provider(
            &provider_lc,
            model_name,
            cheap.base_url.as_deref(),
            cheap.api_key_env.as_deref(),
            explicit_api_key.as_deref(),
            Some(&api_mode),
            pool_ref,
        )
        .map_err(|_| ())?;

        let (command, args) = self.resolve_runtime_command_args(Some(&provider_lc));
        if provider_lc == "copilot-acp"
            && command
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            && !base_url
                .as_deref()
                .map(|u| u.starts_with("acp+tcp://"))
                .unwrap_or(false)
        {
            return Err(());
        }
        if provider_lc == "copilot-acp"
            && !base_url
                .as_deref()
                .map(|u| u.starts_with("acp+tcp://"))
                .unwrap_or(false)
        {
            if let Some(cmd) = command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if which::which(cmd).is_err() {
                    return Err(());
                }
            }
        }
        Ok(ResolvedCheapRuntime {
            model: model_full.to_string(),
            provider: provider_lc,
            base_url,
            api_mode,
            command,
            args,
            credential_pool: if has_runtime_override {
                None
            } else {
                self.primary_credential_pool.clone()
            },
            skip_primary_credential_pool_fallback: has_runtime_override,
        })
    }

    fn resolve_smart_runtime_route(&self, messages: &[Message]) -> Option<TurnRuntimeRoute> {
        let text = self.latest_user_text(messages)?;
        let primary = self.primary_runtime_snapshot();
        let outcome = resolve_turn_route(
            text,
            &self.config.smart_model_routing,
            &primary,
            |cheap, explicit_key| self.try_build_cheap_runtime(cheap, explicit_key),
        );

        match outcome {
            ResolveTurnOutcome::CheapRouted {
                model,
                label,
                runtime,
                signature,
            } => {
                let cheap = self.config.smart_model_routing.cheap_model.as_ref()?;
                Some(TurnRuntimeRoute {
                    model,
                    provider: Some(runtime.provider.clone()),
                    base_url: runtime.base_url.clone(),
                    api_key_env: cheap
                        .api_key_env
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    api_mode: Some(runtime.api_mode.clone()),
                    command: runtime.command.clone(),
                    args: runtime.args.clone(),
                    credential_pool: runtime.credential_pool.clone(),
                    credential_pool_fallback: !runtime.skip_primary_credential_pool_fallback,
                    route_label: Some(label),
                    routing_reason: Some("simple_turn".to_string()),
                    signature,
                })
            }
            ResolveTurnOutcome::Primary { .. } => None,
        }
    }

    /// Resolve the model used for automatic degradation when nearing
    /// `max_cost_usd`.
    fn resolve_cost_degrade_model(&self) -> Option<String> {
        if let Some(ref m) = self.config.cost_guard_degrade_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if let Some(ref m) = self.config.retry.fallback_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if self.config.model.trim() != "openai:gpt-4o-mini" {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(
            "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
             what was accomplished and any remaining tasks.",
        ));
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config.max_tokens,
                self.config.temperature,
                Some(self.config.model.as_str()),
                self.extra_body_for_api_mode(&self.config.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    /// Execute a batch of tool calls in parallel using a JoinSet.
    async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        tool_concurrency: usize,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<hermes_core::ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        let mut join_set = JoinSet::new();
        let tool_concurrency = tool_concurrency.max(1);
        let mut results = Vec::with_capacity(tool_calls.len());
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();

        // Run orchestrated `delegate_task` calls sequentially in the caller's
        // task — this keeps the inner AgentLoop future out of the Send-bound
        // JoinSet and preserves the requested concurrency cap which is already
        // applied upstream via `cap_delegates`.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
            for tc in tool_calls {
                if tc.function.name != "delegate_task" {
                    continue;
                }
                if current_delegate_depth >= max_delegate_depth {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                let parsed: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            format!(
                                "Invalid JSON params for tool 'delegate_task': {}. \
                                 Please retry with valid JSON.",
                                e
                            ),
                        ));
                        continue;
                    }
                };
                let task = parsed
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if task.trim().is_empty() {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        "delegate_task requires non-empty 'task' string.",
                    ));
                    continue;
                }
                let req = crate::sub_agent_orchestrator::SubAgentRequest {
                    task,
                    context: parsed
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    toolset: parsed
                        .get("toolset")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    model: parsed
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    child_depth: current_delegate_depth + 1,
                    max_depth: max_delegate_depth,
                    parent_budget_remaining_usd,
                };
                // Orchestrator internally runs the child on its own
                // `tokio::spawn` task, which erases the child future and breaks
                // async recursion between parent / child `execute_tool_calls`.
                let output = orch.execute(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
            }
        }

        for tc in tool_calls {
            // Skip `delegate_task` when an orchestrator already handled it.
            if orchestrator.is_some() && tc.function.name == "delegate_task" {
                continue;
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            let max_delegate_depth = max_delegate_depth;
            let current_delegate_depth = current_delegate_depth;
            let parent_budget_remaining_usd = parent_budget_remaining_usd;

            join_set.spawn(async move {
                match registry.get(&tool_name) {
                    Some(entry) => {
                        // Parse arguments
                        let mut params: Value = match serde_json::from_str(&raw_args) {
                            Ok(v) => v,
                            Err(e) => {
                                let error_msg = format!(
                                    "Invalid JSON params for tool '{}': {}. \
                                     Please check your parameters and retry with valid JSON.",
                                    tool_name, e
                                );
                                return ToolResult::err(&tool_call_id, error_msg);
                            }
                        };

                        if tool_name == "delegate_task" {
                            if current_delegate_depth >= max_delegate_depth {
                                return ToolResult::err(
                                    &tool_call_id,
                                    format!(
                                        "Delegation depth limit reached ({}/{}).",
                                        current_delegate_depth, max_delegate_depth
                                    ),
                                );
                            }
                            if let Some(obj) = params.as_object_mut() {
                                obj.insert(
                                    "child_depth".to_string(),
                                    Value::from(current_delegate_depth + 1),
                                );
                                obj.insert(
                                    "max_depth".to_string(),
                                    Value::from(max_delegate_depth),
                                );
                                if let Some(remaining) = parent_budget_remaining_usd {
                                    obj.insert(
                                        "parent_budget_remaining_usd".to_string(),
                                        Value::from(remaining),
                                    );
                                }
                            }
                        }

                        // Execute the handler
                        match (entry.handler)(params) {
                            Ok(output) => ToolResult::ok(&tool_call_id, output),
                            Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                        }
                    }
                    None => {
                        let available = registry.names().join(", ");
                        let error_msg = format!(
                            "Unknown tool '{}'. Available tools: [{}]",
                            tool_name, available
                        );
                        ToolResult::err(&tool_call_id, error_msg)
                    }
                }
            });
            if join_set.len() >= tool_concurrency {
                if let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(tool_result) => {
                            if tool_result.is_error {
                                let tc = tool_calls
                                    .iter()
                                    .find(|tc| tc.id == tool_result.tool_call_id);
                                if let Some(tc) = tc {
                                    tool_errors.push(hermes_core::ToolErrorRecord {
                                        tool_name: tc.function.name.clone(),
                                        error: tool_result.content.clone(),
                                        turn,
                                    });
                                }
                            }
                            results.push(tool_result);
                        }
                        Err(e) => {
                            tracing::error!("Task join error: {}", e);
                        }
                    }
                }
            }
        }

        for tool_result in orchestrated {
            if tool_result.is_error {
                let tc = tool_calls
                    .iter()
                    .find(|tc| tc.id == tool_result.tool_call_id);
                if let Some(tc) = tc {
                    tool_errors.push(hermes_core::ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: tool_result.content.clone(),
                        turn,
                    });
                }
            }
            results.push(tool_result);
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => {
                    if tool_result.is_error {
                        // Record the error but we still add the result to context
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(hermes_core::ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    results.push(tool_result);
                }
                Err(e) => {
                    tracing::error!("Task join error: {}", e);
                }
            }
        }

        results
    }

    fn resolve_max_delegate_depth(&self) -> u32 {
        std::env::var("HERMES_MAX_DELEGATE_DEPTH")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(4)
    }

    /// Cap concurrent delegate_task calls based on config.
    fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
        let delegate_count = tool_calls
            .iter()
            .filter(|tc| tc.function.name == "delegate_task")
            .count() as u32;
        if delegate_count > self.config.max_concurrent_delegates {
            tracing::warn!(
                "Capping delegate_task calls from {} to {}",
                delegate_count,
                self.config.max_concurrent_delegates
            );
            let mut kept_delegates = 0u32;
            tool_calls.retain(|tc| {
                if tc.function.name == "delegate_task" {
                    if kept_delegates < self.config.max_concurrent_delegates {
                        kept_delegates += 1;
                        true
                    } else {
                        false
                    }
                } else {
                    true
                }
            });
        }
    }

    fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config.background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermes_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    fn spawn_background_review(&self, turn: u32, ctx: &ContextManager, review_memory_at_end: bool) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config.background_review_enabled {
            return;
        }
        let mut review_skills = false;
        if self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                if c.iters_since_skill >= self.config.skill_creation_nudge_interval {
                    review_skills = true;
                    c.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = self.config.clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.max_turns = cfg.max_turns.min(8);
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let review_cb = self.callbacks.background_review_callback.clone();
        tokio::spawn(async move {
            let agent = AgentLoop::new(cfg, tools, provider);
            match agent.run(hist, None).await {
                Ok(result) => {
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summarize_background_review_result(&result.messages)
                        {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }
}

/// Extract the last user and assistant content from a message slice for memory sync.
fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    let mut actions: Vec<String> = Vec::new();
    for msg in messages {
        if !matches!(msg.role, hermes_core::MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if !data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            continue;
        }
        let lower = message.to_ascii_lowercase();
        if lower.contains("created") || lower.contains("updated") {
            actions.push(message);
        } else if lower.contains("added") || (!target.is_empty() && lower.contains("add")) {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if message.contains("Entry added") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if lower.contains("removed") || lower.contains("replaced") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("💾 {}", deduped.join(" · ")))
}

fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

fn estimate_usage_cost_usd(usage: &UsageStats, model: &str, config: &AgentConfig) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        },
        None => new.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 30);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(!config.skip_context_files);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
        assert_eq!(config.stream_read_max_retries, 2);
    }

    #[test]
    fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    fn classify_error_404_generic_is_retryable() {
        assert_eq!(classify_error("HTTP 404 Not Found"), ErrorClass::Retryable);
        assert_eq!(
            classify_error("gateway returned not found"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_404_model_not_found_is_fatal() {
        assert_eq!(
            classify_error("404 model not found: foo/bar"),
            ErrorClass::Fatal
        );
        assert_eq!(
            classify_error("invalid model: gpt-unknown"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_openrouter_privacy_guardrail_is_fatal() {
        assert_eq!(
            classify_error("HTTP 404: OpenRouter privacy guardrail blocked this endpoint"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_ssl_bad_record_mac_is_retryable() {
        assert_eq!(
            classify_error("[SSL: BAD_RECORD_MAC] sslv3 alert bad record mac (_ssl.c:2580)"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_ssl_openssl_token_form_is_retryable() {
        assert_eq!(
            classify_error("ERR_SSL_SSL/TLS_ALERT_BAD_RECORD_MAC during streaming"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_plain_disconnect_stays_retryable() {
        assert_eq!(
            classify_error("Server disconnected without sending a response"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn maybe_nous_401_diagnostic_returns_hint_for_nous_auth_failures() {
        let diag = maybe_nous_401_diagnostic(
            "nous",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        )
        .expect("nous 401 should produce diagnostics");
        assert!(diag.contains("Nous 401 — Portal authentication failed."));
        assert!(diag.contains("hermes auth login nous"));
        assert!(diag.contains("portal.nousresearch.com"));
        assert!(diag.contains("/tmp/hermes-home/auth.json"));
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_nous_provider() {
        let diag = maybe_nous_401_diagnostic(
            "openrouter",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        );
        assert!(diag.is_none());
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_auth_errors() {
        let diag = maybe_nous_401_diagnostic("nous", "HTTP 500 upstream timeout", None);
        assert!(diag.is_none());
    }

    #[test]
    fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}"),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[test]
    fn status_callback_receives_context_pressure_messages() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    #[tokio::test]
    async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                let finish_reason = if *n == 1 {
                    None
                } else {
                    Some("stop".to_string())
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason,
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    async fn empty_stop_response_is_accepted_without_retry() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct EmptyStopProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for EmptyStopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant(""),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let provider = Arc::new(EmptyStopProvider::default());
        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 3,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone())
            .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        let rows = captured.lock().expect("captured lock");
        assert!(!rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    fn stream_chunk_content(text: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_name(index: u32, id: &str, name: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: Some(id.to_string()),
                    function: Some(hermes_core::FunctionCallDelta {
                        name: Some(name.to_string()),
                        arguments: None,
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_args(index: u32, args: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: None,
                    function: Some(hermes_core::FunctionCallDelta {
                        name: None,
                        arguments: Some(args.to_string()),
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_finish(reason: &str) -> StreamChunk {
        StreamChunk {
            delta: None,
            finish_reason: Some(reason.to_string()),
            usage: None,
        }
    }

    #[derive(Clone, Copy)]
    enum StreamRetryScenario {
        RecoverOnSecondAttempt,
        AlwaysFailMidToolCall,
        TextOnlyDrop,
    }

    struct StreamRetryProvider {
        scenario: StreamRetryScenario,
        calls: std::sync::Mutex<u32>,
    }

    impl StreamRetryProvider {
        fn new(scenario: StreamRetryScenario) -> Self {
            Self {
                scenario,
                calls: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for StreamRetryProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("unused".to_string()))
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            let attempt = *calls;

            let events: Vec<Result<StreamChunk, AgentError>> = match self.scenario {
                StreamRetryScenario::RecoverOnSecondAttempt => {
                    if attempt == 1 {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/x\",")),
                            Err(AgentError::LlmApi(
                                "Stream read error: peer closed connection".to_string(),
                            )),
                        ]
                    } else {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(
                                0,
                                "{\"path\":\"/tmp/x\",\"content\":\"hi\"}",
                            )),
                            Ok(stream_chunk_finish("tool_calls")),
                        ]
                    }
                }
                StreamRetryScenario::AlwaysFailMidToolCall => vec![
                    Ok(stream_chunk_content("Working...")),
                    Ok(stream_chunk_tool_name(0, "call_2", "write_file")),
                    Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/y\",")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection reset by peer".to_string(),
                    )),
                ],
                StreamRetryScenario::TextOnlyDrop => vec![
                    Ok(stream_chunk_content("Partial text")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection lost".to_string(),
                    )),
                ],
            };

            futures::stream::iter(events).boxed()
        }
    }

    #[tokio::test]
    async fn stream_mid_tool_call_silent_retry_recovers_tool_call() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::RecoverOnSecondAttempt,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let StreamCollectOutcome::Complete(resp) = out.expect("stream should recover") else {
            panic!("expected complete response");
        };
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("missing tool call");
        assert_eq!(tc.function.name, "write_file");
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_mid_tool_call_exhausted_retries_returns_error() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::AlwaysFailMidToolCall,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("should fail after retries"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_text_only_drop_does_not_retry() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::TextOnlyDrop));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("text-only stream drops should not retry silently"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        assert!(!seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[test]
    fn quiet_mode_suppresses_status_callback() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    fn test_builtin_personality_injected_into_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    fn test_unknown_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn test_smart_model_routing_cheap_route_for_simple_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("帮我总结一下今天要做什么")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = agent.primary_runtime_snapshot();
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                api_key_env: None,
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    fn test_runtime_provider_stepfun_build_supported() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "stepfun".to_string(),
            RuntimeProviderConfig {
                api_key: Some("stepfun-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let built =
            agent.build_runtime_provider("stepfun", "step-3.5-flash", None, None, None, None, None);
        assert!(built.is_ok(), "stepfun runtime provider should build");
    }

    #[test]
    fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // Fixture-style cases distilled from Python v2026.4.13:
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = agent.evolution_counters.lock().expect("counter lock");
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp://copilot".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    fn test_smart_model_routing_skips_complex_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert!(selected.is_none());
    }

    #[test]
    fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
                extra_content: None,
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    fn test_hydrate_session_search_args_injects_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
            extra_content: None,
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
            extra_content: None,
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        let max = agent.config.max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost: Some(0.01),
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: Some(0.02),
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: None,
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            estimated_cost: None,
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

    /// Smoke test: config-centre OAuth metadata wins over env fallback, and
    /// env is used when config is empty. Mirrors the Python behaviour of
    /// `resolve_runtime_provider_credentials` where provider config takes
    /// precedence over environment lookup.
    #[test]
    fn test_oauth_refresh_config_prefers_provider_config_over_env() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/token".to_string()),
                oauth_client_id: Some("cfg-client".to_string()),
            },
        );
        // An unknown provider reachable only via config (env fallback is gated
        // on known providers, so this exercises the cfg_token_url.zip path).
        runtime_providers.insert(
            "custom-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/custom-token".to_string()),
                oauth_client_id: Some("custom-client".to_string()),
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // Set conflicting env values — config must win.
        std::env::set_var("HERMES_QWEN_OAUTH_TOKEN_URL", "https://env.example.com/tok");
        std::env::set_var("HERMES_QWEN_OAUTH_CLIENT_ID", "env-client");

        let (token_url, client_id) = agent.oauth_refresh_config("qwen-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/token");
        assert_eq!(client_id, "cfg-client");

        // Unknown-provider path still resolves when config centre supplies both.
        let (token_url, client_id) = agent.oauth_refresh_config("custom-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/custom-token");
        assert_eq!(client_id, "custom-client");

        std::env::remove_var("HERMES_QWEN_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_QWEN_OAUTH_CLIENT_ID");
    }

    #[test]
    fn test_runtime_provider_api_key_env_is_resolved() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "custom".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        std::env::set_var("MY_FALLBACK_KEY", "env-secret");
        let resolved = agent.resolve_runtime_api_key("custom", None, None);
        assert_eq!(resolved.as_deref(), Some("env-secret"));
        std::env::remove_var("MY_FALLBACK_KEY");
    }

    #[test]
    fn test_runtime_provider_api_key_env_supports_anthropic_aliases_and_gemini_oauth() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_TOKEN");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "claude-code-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("anthropic", None, None)
                .as_deref(),
            Some("claude-code-token")
        );
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        std::env::set_var("HERMES_GEMINI_OAUTH_API_KEY", "gemini-oauth-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("google-gemini-cli", None, None)
                .as_deref(),
            Some("gemini-oauth-token")
        );
        std::env::remove_var("HERMES_GEMINI_OAUTH_API_KEY");
    }

    #[test]
    fn test_oauth_refresh_config_anthropic_defaults_available() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("anthropic").unwrap();
        assert_eq!(token_url, "https://console.anthropic.com/v1/oauth/token");
        assert_eq!(client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[test]
    fn test_runtime_provider_stepfun_env_key_and_base_url_defaults() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        std::env::remove_var("HERMES_STEPFUN_API_KEY");
        std::env::set_var("STEPFUN_API_KEY", "stepfun-secret");
        let resolved = agent.resolve_runtime_api_key("stepfun", None, None);
        assert_eq!(resolved.as_deref(), Some("stepfun-secret"));
        std::env::remove_var("STEPFUN_API_KEY");

        let base = agent.resolve_runtime_base_url("stepfun", None);
        assert_eq!(base.as_deref(), Some("https://api.stepfun.ai/step_plan/v1"));
    }

    #[test]
    fn test_governor_reduces_budget_under_high_pressure() {
        let mut ctx = ContextManager::default_budget();
        let payload = "x".repeat(((ctx.max_context_chars() as f64) * 0.9) as usize);
        ctx.add_message(Message::user(payload));
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let gov = governor_for_turn(&config, &ctx, 12);
        assert!(gov.pressure >= 0.9);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 4);
    }

    #[test]
    fn test_redact_json_value_masks_sensitive_fields() {
        let mut payload = serde_json::json!({
            "api_key": "abc",
            "nested": { "token": "def", "safe": "ok" },
            "list": [{"password":"x"}, {"value":"y"}]
        });
        redact_json_value(&mut payload);
        assert_eq!(payload["api_key"], "[redacted]");
        assert_eq!(payload["nested"]["token"], "[redacted]");
        assert_eq!(payload["nested"]["safe"], "ok");
        assert_eq!(payload["list"][0]["password"], "[redacted]");
    }
}
