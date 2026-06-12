//! Agent configuration types and default-value functions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use hermes_core::{AgentError, BudgetConfig, Message, UsageStats};

use crate::prompt_builder::TOOL_USE_ENFORCEMENT_MODELS;
pub use crate::smart_model_routing::{ApiMode, SmartModelRoutingConfig};

// ---------------------------------------------------------------------------
// RetryConfig
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
    /// Optional ordered failover chain. Entries are attempted in order after retries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
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
            fallback_models: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// RuntimeProviderConfig
// ---------------------------------------------------------------------------

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
    /// Per-provider HTTP request timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_seconds: Option<f64>,
    /// Optional provider-specific wire protocol override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_mode: Option<ApiMode>,
}

// ---------------------------------------------------------------------------
// OAuthStoreCredential
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OAuthStoreCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    pub(crate) access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    #[serde(default)]
    pub(crate) expires_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// CompactionGovernanceMode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionGovernanceMode {
    Off,
    Advisory,
    Enforce,
}

impl CompactionGovernanceMode {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disable" | "disabled" | "0" => Some(Self::Off),
            "on" | "advisory" | "warn" | "1" => Some(Self::Advisory),
            "enforce" | "strict" => Some(Self::Enforce),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

pub(crate) fn compaction_governance_mode_runtime() -> CompactionGovernanceMode {
    std::env::var("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE")
        .ok()
        .as_deref()
        .and_then(CompactionGovernanceMode::parse)
        .unwrap_or(CompactionGovernanceMode::Advisory)
}

pub(crate) fn contextlattice_orchestration_script_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HERMES_CONTEXTLATTICE_ORCH_SCRIPT") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let default =
        PathBuf::from("/Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py");
    if default.exists() {
        return Some(default);
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("scripts").join("agent_orchestration.py");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn should_inject_tool_enforcement_for_model(_model: &str) -> bool {
    let disabled = std::env::var("HERMES_DISABLE_TOOL_ENFORCEMENT_PROMPT")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    if disabled {
        return false;
    }

    let model_lower = _model.to_ascii_lowercase();
    TOOL_USE_ENFORCEMENT_MODELS
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| model_lower.contains(pattern))
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

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

    /// API mode - selects the request format for the LLM provider.
    #[serde(default)]
    pub api_mode: ApiMode,

    /// Python `model.openai_runtime` — when `codex_app_server`, OpenAI/Codex providers use
    /// [`ApiMode::CodexAppServer`] (see [`crate::smart_model_routing::maybe_apply_codex_app_server_runtime`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_runtime: Option<String>,

    /// Ollama runtime `num_ctx` for local models (Python `agent._ollama_num_ctx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_num_ctx: Option<u32>,

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

    /// Maximum sub-agent spawn depth. Values below 1 are normalized to 1.
    #[serde(default = "default_max_delegate_depth")]
    pub max_delegate_depth: u32,

    /// Optional config-level model override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_model: Option<String>,

    /// Optional config-level provider override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_provider: Option<String>,

    /// Optional config-level base URL override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_base_url: Option<String>,

    /// Optional config-level API key override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_api_key: Option<String>,

    /// Ephemeral messages prepended each turn (visible to model, not persisted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefill_messages: Vec<Message>,

    /// Flush memories every N turns.
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,

    /// Session identifier - used for memory and persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Stable gateway chat key for context-engine `conversation_id` (Python `_gateway_session_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_session_key: Option<String>,

    /// HERMES_HOME path - used by memory plugins for config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes_home: Option<String>,

    /// Skip memory integration even if a MemoryManager is provided.
    #[serde(default)]
    pub skip_memory: bool,

    /// Local user interest (POI) topic store configuration.
    #[serde(default)]
    pub interest: hermes_config::InterestConfig,

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

    /// Optional allow-list for skill discovery/injection. Empty means allow all.
    #[serde(default)]
    pub enabled_skills: Vec<String>,

    /// Optional deny-list for skill discovery/injection.
    #[serde(default)]
    pub disabled_skills: Vec<String>,

    /// Include session_id in system prompt timestamp block.
    #[serde(default)]
    pub pass_session_id: bool,

    /// Inject universal "Finishing the job" guidance into system prompt.
    #[serde(default = "default_task_completion_guidance")]
    pub task_completion_guidance: bool,

    /// Runtime provider credentials/endpoints keyed by provider name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub runtime_providers: HashMap<String, RuntimeProviderConfig>,

    /// OpenRouter provider routing: allowed slugs only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers_allowed: Vec<String>,

    /// OpenRouter provider routing: ignored slugs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers_ignored: Vec<String>,

    /// OpenRouter provider routing: preferred order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers_order: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_sort: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_require_parameters: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data_collection: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter_min_coding_score: Option<f64>,

    /// Enable filesystem checkpoints before mutating tools.
    #[serde(default = "default_checkpoints_enabled")]
    pub checkpoints_enabled: bool,

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

    /// Proactive session recall at turn start (Recall Planner).
    #[serde(default = "default_recall_enabled")]
    pub recall_enabled: bool,

    /// Emit background review metrics snapshots (`debug` tracing only).
    /// Child sessions disable this for stricter quiet-mode parity.
    #[serde(default = "default_background_review_metrics_enabled")]
    pub background_review_metrics_enabled: bool,

    /// Persist structured background review events to `$HERMES_HOME/evolution/reviews.jsonl`.
    #[serde(default = "default_evolution_ledger_enabled")]
    pub evolution_ledger_enabled: bool,

    /// Max JSONL lines retained in the review ledger (`0` = unlimited).
    #[serde(default = "default_evolution_ledger_max_entries")]
    pub evolution_ledger_max_entries: u32,

    /// Exact system prompt from SQLite when continuing a session (stable Anthropic prefix cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_system_prompt: Option<String>,

    /// Anthropic prompt caching TTL tier (`"5m"` or `"1h"`).
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: String,

    /// Inject `cache_control` breakpoints on outbound API messages.
    #[serde(default)]
    pub use_prompt_caching: bool,

    /// Place markers on inner content blocks (native Anthropic) vs message envelope (OpenRouter).
    #[serde(default)]
    pub use_native_cache_layout: bool,

    /// Progress ratio (0-1) at which Python emits a *caution* budget nudge (`_budget_caution_threshold`).
    #[serde(default = "default_budget_caution_threshold")]
    pub budget_caution_threshold: f64,

    /// Progress ratio (0-1) at which Python emits an urgent budget warning (`_budget_warning_threshold`).
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

    /// Max retries when the model returns truncated tool arguments (`finish_reason=length` parity).
    #[serde(default = "default_truncated_tool_call_max_retries")]
    pub truncated_tool_call_max_retries: u32,

    /// Max retries when assistant output is incomplete due to `finish_reason=length` or `pause_turn`.
    #[serde(default = "default_continuation_max_retries")]
    pub continuation_max_retries: u32,

    /// Max retries for codex-style intermediate ack continuation nudges.
    #[serde(default = "default_ack_continuation_max_retries")]
    pub ack_continuation_max_retries: u32,

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

    /// Adaptive web research planner/evaluator and per-message tool budgets.
    #[serde(default)]
    pub web_research: hermes_config::WebResearchConfig,

    /// Persist turn trajectories to JSONL (Python `save_trajectories`).
    #[serde(default)]
    pub save_trajectories: bool,
}

// ---------------------------------------------------------------------------
// Default-value functions for AgentConfig fields
// ---------------------------------------------------------------------------

fn default_max_turns() -> u32 {
    250
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

fn default_max_concurrent_delegates() -> u32 {
    1
}

fn default_max_delegate_depth() -> u32 {
    4
}

fn default_memory_flush_interval() -> u32 {
    5
}

fn default_task_completion_guidance() -> bool {
    true
}

fn default_cost_guard_degrade_at_ratio() -> f64 {
    0.8
}

fn default_checkpoint_interval_turns() -> u32 {
    3
}

fn default_checkpoints_enabled() -> bool {
    true
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

fn default_recall_enabled() -> bool {
    true
}

fn default_evolution_ledger_enabled() -> bool {
    true
}

fn default_evolution_ledger_max_entries() -> u32 {
    200
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

fn default_continuation_max_retries() -> u32 {
    3
}

fn default_ack_continuation_max_retries() -> u32 {
    2
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

fn default_cache_ttl() -> String {
    "5m".to_string()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            budget: BudgetConfig::default(),
            model: default_model(),
            api_mode: ApiMode::default(),
            openai_runtime: None,
            ollama_num_ctx: None,
            retry: RetryConfig::default(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            quiet_mode: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            max_delegate_depth: default_max_delegate_depth(),
            delegation_model: None,
            delegation_provider: None,
            delegation_base_url: None,
            delegation_api_key: None,
            prefill_messages: Vec::new(),
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            gateway_session_key: None,
            hermes_home: None,
            skip_memory: false,
            interest: hermes_config::InterestConfig::default(),
            skip_context_files: false,
            smart_model_routing: SmartModelRoutingConfig::default(),
            provider: None,
            platform: None,
            enabled_skills: Vec::new(),
            disabled_skills: Vec::new(),
            pass_session_id: false,
            task_completion_guidance: default_task_completion_guidance(),
            runtime_providers: HashMap::new(),
            providers_allowed: Vec::new(),
            providers_ignored: Vec::new(),
            providers_order: Vec::new(),
            provider_sort: None,
            provider_require_parameters: None,
            provider_data_collection: None,
            openrouter_min_coding_score: None,
            checkpoints_enabled: default_checkpoints_enabled(),
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
            recall_enabled: default_recall_enabled(),
            background_review_metrics_enabled: default_background_review_metrics_enabled(),
            evolution_ledger_enabled: default_evolution_ledger_enabled(),
            evolution_ledger_max_entries: default_evolution_ledger_max_entries(),
            stored_system_prompt: None,
            cache_ttl: default_cache_ttl(),
            use_prompt_caching: false,
            use_native_cache_layout: false,
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
            continuation_max_retries: default_continuation_max_retries(),
            ack_continuation_max_retries: default_ack_continuation_max_retries(),
            code_index_enabled: default_code_index_enabled(),
            code_index_max_files: default_code_index_max_files(),
            code_index_max_symbols: default_code_index_max_symbols(),
            lsp_context_enabled: default_lsp_context_enabled(),
            lsp_context_max_chars: default_lsp_context_max_chars(),
            web_research: hermes_config::WebResearchConfig::default(),
            save_trajectories: false,
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
// FinalizationSignals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct FinalizationSignals {
    pub(crate) finish_reason: Option<String>,
    pub(crate) has_tool_calls: bool,
    pub(crate) has_visible_text: bool,
    pub(crate) has_visible_text_after_think: bool,
    pub(crate) has_reasoning: bool,
    pub(crate) continuation_required: bool,
    pub(crate) ack_detected: bool,
}

impl FinalizationSignals {
    pub(crate) fn final_gate_passed(&self) -> bool {
        !self.has_tool_calls && !self.continuation_required && !self.ack_detected
    }
}

// ---------------------------------------------------------------------------
// Evolution counters (Python `_turns_since_memory` / `_iters_since_skill`)
// ---------------------------------------------------------------------------

/// Session-scoped counters for memory / skill nudges (mirrors Python `AIAgent` fields).
#[derive(Debug, Default)]
pub struct EvolutionCounters {
    /// User turns in this session (Python `_user_turn_count`).
    pub user_turn_count: u32,
    pub turns_since_memory: u32,
    pub iters_since_skill: u32,
}

// ---------------------------------------------------------------------------
// ErrorClass
// ---------------------------------------------------------------------------

/// Classify an API error for retry/failover decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Retryable,
    RateLimit,
    ContextOverflow,
    Auth,
    Fatal,
}

// ---------------------------------------------------------------------------
// Error classification helpers
// ---------------------------------------------------------------------------

pub(crate) fn has_ssl_transient_phrase(lower: &str) -> bool {
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

pub(crate) fn is_stream_not_supported_error(err: &AgentError) -> bool {
    let msg = match err {
        AgentError::LlmApi(m)
        | AgentError::Gateway(m)
        | AgentError::Io(m)
        | AgentError::Config(m) => m.as_str(),
        AgentError::AuthFailed(m) => m.as_str(),
        _ => return false,
    };
    let lower = msg.to_ascii_lowercase();
    lower.contains("stream") && lower.contains("not supported")
}

pub(crate) fn is_copilot_acp_transport(provider: &str, base_url: &str) -> bool {
    let p = provider.trim().to_ascii_lowercase();
    let u = base_url.trim().to_ascii_lowercase();
    p == "copilot-acp" || u.starts_with("acp://copilot") || u.starts_with("acp+tcp://")
}

pub(crate) fn is_transient_stream_error(err: &AgentError) -> bool {
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

pub(crate) fn rand_u64_range(min: u64, max: u64) -> u64 {
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
