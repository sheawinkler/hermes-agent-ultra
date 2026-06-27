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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Per-request timeout in seconds for this provider transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_seconds: Option<f64>,
    /// Optional provider-specific wire protocol override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_mode: Option<ApiMode>,
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

const CONVERSATIONAL_SUPPORT_GUIDANCE: &str = "# Conversational support protocol\nWhen users share personal stress, emotions, or difficult decisions, start with a brief non-judgmental acknowledgment, ask one clarifying question if context is missing, then offer practical options with trade-offs. Keep factual or technical requests direct and do not force emotional language where it does not fit. Do not present yourself as a therapist or crisis service; when safety risk appears, urge the user to seek immediate professional or emergency help.";
const OAUTH_REFRESH_BACKOFF_SECS: u64 = 60;
const SESSION_OBJECTIVE_PREFIX: &str = "[SESSION_OBJECTIVE] ";
const OBJECTIVE_PATCH_TAG: &str = "PATCH_VERIFIED:";
const OBJECTIVE_ANALYTICS_TAG: &str = "ANALYTICS_VERIFIED:";
const OBJECTIVE_DEEP_AUDIT_TAG: &str = "DEEP_AUDIT_VERIFIED:";
const OBJECTIVE_GUARD_MAX_RETRIES: u32 = 2;
const OBJECTIVE_DEEP_AUDIT_MAX_RETRIES: u32 = 4;
const OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS: usize = 2;
const OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES: usize = 5;
const OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS: usize = 3;
const OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS: usize = 3;
const FINALIZER_EVIDENCE_MAX_RETRIES: u32 = 2;
const FINALIZER_OUTPUT_QUALITY_MAX_RETRIES: u32 = 2;
const FINALIZER_ACTION_EXECUTION_MAX_RETRIES: u32 = 2;
const FINALIZER_WEB_RESEARCH_MAX_RETRIES: u32 = 4;
const FINALIZER_GOOGLE_WORKSPACE_MAX_RETRIES: u32 = 2;
const FINALIZER_TASK_FOCUS_MAX_RETRIES: u32 = 2;
const FINALIZER_REPO_RESEARCH_PLAN_MAX_RETRIES: u32 = 2;
const REPO_RESEARCH_MIN_WORKSTREAMS: usize = 3;
const REPO_RESEARCH_MIN_UNIQUE_FILES: usize = 2;
const REPO_RESEARCH_MIN_COMMANDS: usize = 2;

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

const TOOL_USE_ENFORCEMENT_GUIDANCE: &str = "# Tool-use enforcement\nUse tools whenever they are necessary to verify facts, inspect code/files, or execute requested actions. Do not describe an action without making the corresponding tool call in the same response. Do not call tools only to satisfy policy or to emit no-op commands. If the request is fully answerable without tools, return a direct final answer. Avoid repetitive tool loops: if additional tool calls will not add new evidence, stop and provide the best grounded final result.";
const CONTEXTLATTICE_OPERATIONAL_GUIDANCE: &str = "# ContextLattice operational guidance\nWhen a user asks to confirm, connect, verify, or harden ContextLattice integration, do not answer from assumptions. First check local integration instructions when present via env `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH`. Then attempt ContextLattice tool calls: use `contextlattice_search` for a direct probe and `contextlattice_context_pack` when broader grounding is needed. Use `contextlattice_write` for checkpoints. If a call fails, report the concrete error and provide the exact remediation steps. Never run shell command `contextlattice` for this workflow; use the ContextLattice tools directly. Do not claim lack of access before attempting at least one ContextLattice tool call in the current turn.";
const CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL: &str = "http://127.0.0.1:8075";
const CONTEXTLATTICE_COMPACTION_FILE: &str = "notes/hermes-agent-compaction.md";

const OPENAI_MODEL_EXECUTION_GUIDANCE: &str = "# Execution discipline (OpenAI)\nUse tools whenever they improve correctness, completeness, or grounding. Do not stop early when another tool call would materially improve the result. Verify outcomes before declaring completion.";

const GOOGLE_MODEL_OPERATIONAL_GUIDANCE: &str = "# Operational guidance (Google)\nBe concise and execution-first. Prefer absolute paths, parallel tool calls when safe, and verify each substantive change.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionGovernanceMode {
    Off,
    Advisory,
    Enforce,
}

impl CompactionGovernanceMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disable" | "disabled" | "0" => Some(Self::Off),
            "on" | "advisory" | "warn" | "1" => Some(Self::Advisory),
            "enforce" | "strict" => Some(Self::Enforce),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

fn compaction_governance_mode_runtime() -> CompactionGovernanceMode {
    std::env::var("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE")
        .ok()
        .as_deref()
        .and_then(CompactionGovernanceMode::parse)
        .unwrap_or(CompactionGovernanceMode::Advisory)
}

fn contextlattice_orchestrator_url_runtime() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|_| CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn contextlattice_timeout_runtime() -> Duration {
    let seconds = std::env::var("HERMES_CONTEXTLATTICE_TIMEOUT_SECS")
        .or_else(|_| std::env::var("CONTEXTLATTICE_TIMEOUT_SECS"))
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(10.0)
        .clamp(1.0, 60.0);
    Duration::from_secs_f64(seconds)
}

fn write_contextlattice_checkpoint(
    topic_path: &str,
    file_name: &str,
    content: &str,
) -> Result<(), String> {
    let url = format!("{}/memory/write", contextlattice_orchestrator_url_runtime());
    let payload = json!({
        "projectName": "hermes-agent-ultra",
        "fileName": file_name,
        "topicPath": topic_path,
        "content": content,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(contextlattice_timeout_runtime())
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client.post(&url).json(&payload);
    if let Ok(api_key) = std::env::var("CONTEXTLATTICE_API_KEY") {
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(api_key);
        }
    }
    let response = request.send().map_err(|err| err.to_string())?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    let preview: String = body.chars().take(240).collect();
    Err(format!("HTTP {status}: {}", preview.trim()))
}

fn should_inject_tool_enforcement_for_model(_model: &str) -> bool {
    let disabled = std::env::var("HERMES_DISABLE_TOOL_ENFORCEMENT_PROMPT")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);
    !disabled
}

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

    /// Coding-context posture mode: auto/focus/on/off.
    #[serde(default = "default_coding_context")]
    pub coding_context: String,

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

    /// Runtime provider credentials/endpoints keyed by provider name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub runtime_providers: HashMap<String, RuntimeProviderConfig>,

    /// Ephemeral system prompt appended at API-call time only.
    /// This is intentionally not persisted in context history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_system_prompt: Option<String>,

    /// Ephemeral few-shot/prefill messages injected into provider requests.
    /// These are model-visible but stripped from returned/persisted history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefill_messages: Vec<Message>,

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
    250
}

fn unlimited_turns_enabled() -> bool {
    std::env::var("HERMES_MAX_TURNS_UNLIMITED")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn effective_max_turns(config_max_turns: u32) -> Option<u32> {
    if unlimited_turns_enabled() || config_max_turns == 0 {
        None
    } else {
        Some(config_max_turns)
    }
}

fn default_model() -> String {
    "gpt-5.5".to_string()
}

fn default_max_concurrent_delegates() -> u32 {
    1
}

fn default_max_delegate_depth() -> u32 {
    4
}

fn normalize_delegate_depth(value: u32) -> u32 {
    value.max(1)
}

fn parse_delegate_depth(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<i128>() {
        Ok(parsed) if parsed < 1 => Some(1),
        Ok(parsed) => Some(parsed.min(i128::from(u32::MAX)) as u32),
        Err(_) => None,
    }
}

fn delegation_spawning_paused() -> bool {
    std::env::var("HERMES_DELEGATION_PAUSED")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
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

fn default_coding_context() -> String {
    "auto".to_string()
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
            max_delegate_depth: default_max_delegate_depth(),
            delegation_model: None,
            delegation_provider: None,
            delegation_base_url: None,
            delegation_api_key: None,
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            hermes_home: None,
            skip_memory: false,
            skip_context_files: false,
            coding_context: default_coding_context(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            provider: None,
            platform: None,
            enabled_skills: Vec::new(),
            disabled_skills: Vec::new(),
            pass_session_id: false,
            runtime_providers: HashMap::new(),
            ephemeral_system_prompt: None,
            prefill_messages: Vec::new(),
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

