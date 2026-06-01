//! Gateway configuration: the top-level config struct and its sub-types.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use hermes_core::BudgetConfig;

use crate::platform::PlatformConfig;
use crate::session::SessionConfig;
use crate::streaming::StreamingConfig;

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Top-level configuration for the hermes gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Default LLM model identifier (e.g. "gpt-4o", "claude-3-opus").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Personality / persona name to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Maximum agent conversation turns before forced stop.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Custom system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// List of enabled tool names. Defaults to all core tools.
    #[serde(default = "default_tools")]
    pub tools: Vec<String>,

    /// Budget limits for tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Per-platform configuration (keyed by platform name, e.g. "discord").
    #[serde(default)]
    pub platforms: HashMap<String, PlatformConfig>,

    /// Per-platform toolset selection (e.g. cli/hermes-cli, telegram/hermes-telegram).
    #[serde(default = "default_platform_toolsets")]
    pub platform_toolsets: HashMap<String, Vec<String>>,

    /// Session management settings.
    #[serde(default)]
    pub session: SessionConfig,

    /// Session persistence database maintenance (auto-prune + VACUUM).
    #[serde(default)]
    pub sessions: SessionsMaintenanceConfig,

    /// Streaming / progressive-output settings.
    #[serde(default)]
    pub streaming: StreamingConfig,

    /// Terminal / command-execution backend settings.
    #[serde(default)]
    pub terminal: TerminalConfig,

    /// Named LLM provider configurations.
    #[serde(default)]
    pub llm_providers: HashMap<String, LlmProviderConfig>,

    /// Legacy single fallback model spec, e.g. `openrouter:anthropic/claude-sonnet-4.6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,

    /// Ordered fallback model specs tried after primary retries fail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,

    /// Optional per-turn smart model routing (cheap-vs-strong).
    #[serde(default)]
    pub smart_model_routing: SmartModelRoutingConfig,

    /// Per-task auxiliary model/direct-endpoint overrides.
    ///
    /// Keys match the user-facing `auxiliary.<task>.*` config.yaml surface
    /// used by side tasks such as `vision`, `web_extract`, and `approval`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub auxiliary: BTreeMap<String, AuxiliaryTaskConfig>,

    /// User-defined slash commands that bypass the agent loop.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quick_commands: BTreeMap<String, QuickCommandConfig>,

    /// Upstream-compatible TTS configuration block.
    ///
    /// Kept as structured JSON because upstream accepts provider-specific
    /// nested maps (`tts.openai`, `tts.providers.<name>`, `tts.piper`, etc.).
    /// Runtime consumers validate the subkeys they understand and ignore the
    /// rest so future upstream TTS knobs do not get dropped during config
    /// round-trips.
    #[serde(default, skip_serializing_if = "is_json_null")]
    pub tts: serde_json::Value,

    /// Optional HTTP/SOCKS proxy settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Approval / safety-gate settings.
    #[serde(default)]
    pub approval: ApprovalConfig,

    /// Security policy toggles.
    #[serde(default)]
    pub security: SecurityConfig,

    /// Skills enable/disable configuration.
    #[serde(default)]
    pub skills: SkillsSettings,

    /// Tools enable/disable and per-tool configuration.
    #[serde(default)]
    pub tools_config: ToolsSettings,

    /// MCP server connection configuration.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,

    /// Profile system: selected profile and named profile files.
    #[serde(default)]
    pub profile: ProfileConfig,

    /// Agent loop nudges + background review (parity with Python `memory` / `skills` cadence).
    #[serde(default)]
    pub agent: AgentLoopBehaviorConfig,

    /// Override for the hermes home directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            model: None,
            personality: None,
            max_turns: default_max_turns(),
            system_prompt: None,
            tools: default_tools(),
            budget: BudgetConfig::default(),
            platforms: HashMap::new(),
            platform_toolsets: default_platform_toolsets(),
            session: SessionConfig::default(),
            sessions: SessionsMaintenanceConfig::default(),
            streaming: StreamingConfig::default(),
            terminal: TerminalConfig::default(),
            llm_providers: HashMap::new(),
            fallback_model: None,
            fallback_models: Vec::new(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            auxiliary: BTreeMap::new(),
            quick_commands: BTreeMap::new(),
            tts: serde_json::Value::Null,
            proxy: None,
            approval: ApprovalConfig::default(),
            security: SecurityConfig::default(),
            skills: SkillsSettings::default(),
            tools_config: ToolsSettings::default(),
            mcp_servers: Vec::new(),
            profile: ProfileConfig::default(),
            agent: AgentLoopBehaviorConfig::default(),
            home_dir: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickCommandConfig {
    /// Command kind. Supported runtime kinds: `exec` and `alias`.
    #[serde(
        default = "default_quick_command_type",
        rename = "type",
        alias = "kind"
    )]
    pub kind: String,

    /// Shell command to run for `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Slash command target for `alias`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Optional execution timeout in seconds. Defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Back-compat alias for `timeout_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

impl Default for QuickCommandConfig {
    fn default() -> Self {
        Self {
            kind: default_quick_command_type(),
            command: None,
            target: None,
            timeout_secs: None,
            timeout: None,
        }
    }
}

impl QuickCommandConfig {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.or(self.timeout).unwrap_or(30)
    }
}

fn default_quick_command_type() -> String {
    "exec".to_string()
}

fn is_json_null(value: &serde_json::Value) -> bool {
    value.is_null()
}

/// User-facing override for one auxiliary side task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuxiliaryTaskConfig {
    /// `auto` means resolve through the standard auxiliary chain.
    #[serde(default = "default_auxiliary_provider")]
    pub provider: String,
    /// Empty means use the selected provider's default auxiliary model.
    #[serde(default)]
    pub model: String,
    /// Direct OpenAI-compatible endpoint. When set, it takes precedence over provider.
    #[serde(default)]
    pub base_url: String,
    /// API key for a direct endpoint or explicit task provider.
    #[serde(default)]
    pub api_key: String,
    /// Per-attempt timeout in seconds. Accepts both `timeout` and legacy `timeout_secs`.
    #[serde(
        default,
        alias = "timeout_secs",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<u64>,
    /// Provider-specific OpenAI-compatible request body additions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    /// Vision-only image download timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_timeout: Option<u64>,
    /// Preserve unknown future task keys without dropping user config.
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for AuxiliaryTaskConfig {
    fn default() -> Self {
        Self {
            provider: default_auxiliary_provider(),
            model: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            timeout: None,
            extra_body: None,
            download_timeout: None,
            extra: BTreeMap::new(),
        }
    }
}

impl AuxiliaryTaskConfig {
    pub fn with_timeout(mut self, timeout: u64) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_download_timeout(mut self, timeout: u64) -> Self {
        self.download_timeout = Some(timeout);
        self
    }
}

fn default_auxiliary_provider() -> String {
    "auto".to_string()
}

/// Upstream-shaped default auxiliary task table used by setup/config UIs.
///
/// `GatewayConfig::default()` intentionally keeps `auxiliary` empty so normal
/// config layering does not treat defaults as user overrides. Runtime
/// resolution still defaults each missing task to provider=`auto`, model=`""`.
pub fn default_auxiliary_task_configs() -> BTreeMap<String, AuxiliaryTaskConfig> {
    let mut tasks = BTreeMap::new();
    tasks.insert(
        "vision".to_string(),
        AuxiliaryTaskConfig::default()
            .with_timeout(120)
            .with_download_timeout(30),
    );
    tasks.insert(
        "web_extract".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(360),
    );
    tasks.insert(
        "compression".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(120),
    );
    tasks.insert(
        "skills_hub".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(30),
    );
    tasks.insert(
        "approval".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(30),
    );
    tasks.insert(
        "mcp".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(30),
    );
    tasks.insert(
        "title_generation".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(30),
    );
    tasks.insert(
        "triage_specifier".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(120),
    );
    tasks.insert(
        "kanban_decomposer".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(180),
    );
    tasks.insert(
        "profile_describer".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(60),
    );
    tasks.insert(
        "curator".to_string(),
        AuxiliaryTaskConfig::default().with_timeout(600),
    );
    tasks
}

const BUILTIN_AUXILIARY_ENV_BRIDGE_TASKS: &[&str] = &["approval", "vision", "web_extract"];

impl GatewayConfig {
    /// Return config-derived environment overrides for the built-in auxiliary
    /// bridge set (`vision`, `web_extract`, `approval`).
    pub fn builtin_auxiliary_env_overrides(&self) -> Vec<(String, String)> {
        self.auxiliary_env_overrides_for(std::iter::empty::<&str>())
    }

    /// Return config-derived `AUXILIARY_<TASK>_*` assignments for built-ins
    /// plus caller-provided plugin task keys.
    ///
    /// The helper mirrors Python's gateway/CLI bridge contract without forcing
    /// Rust runtime code to rely on process-global env mutation.
    pub fn auxiliary_env_overrides_for<I, S>(&self, extra_task_keys: I) -> Vec<(String, String)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut bridged: BTreeSet<String> = BUILTIN_AUXILIARY_ENV_BRIDGE_TASKS
            .iter()
            .map(|task| (*task).to_string())
            .collect();
        for task in extra_task_keys {
            let normalized = normalize_auxiliary_task_key(task.as_ref());
            if !normalized.is_empty() {
                bridged.insert(normalized);
            }
        }

        let mut overrides = Vec::new();
        for task_key in bridged {
            let Some(task_cfg) = self.auxiliary.get(task_key.as_str()) else {
                continue;
            };
            push_auxiliary_task_env_overrides(&mut overrides, &task_key, task_cfg);
        }
        overrides
    }
}

fn normalize_auxiliary_task_key(task: &str) -> String {
    task.trim().to_ascii_lowercase()
}

fn auxiliary_task_env_suffix(task: &str) -> String {
    task.trim().to_ascii_uppercase()
}

fn push_auxiliary_task_env_overrides(
    overrides: &mut Vec<(String, String)>,
    task_key: &str,
    task_cfg: &AuxiliaryTaskConfig,
) {
    let upper = auxiliary_task_env_suffix(task_key);
    let provider = task_cfg.provider.trim();
    if !provider.is_empty() && !provider.eq_ignore_ascii_case("auto") {
        overrides.push((format!("AUXILIARY_{upper}_PROVIDER"), provider.to_string()));
    }
    for (field, value) in [
        ("MODEL", task_cfg.model.trim()),
        ("BASE_URL", task_cfg.base_url.trim()),
        ("API_KEY", task_cfg.api_key.trim()),
    ] {
        if !value.is_empty() {
            overrides.push((format!("AUXILIARY_{upper}_{field}"), value.to_string()));
        }
    }
}

/// SQLite session persistence maintenance settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionsMaintenanceConfig {
    /// Enable automatic prune/VACUUM sweeps at startup.
    #[serde(default)]
    pub auto_prune: bool,
    /// Keep sessions updated within the last N days.
    #[serde(default = "default_sessions_retention_days")]
    pub retention_days: u32,
    /// Run VACUUM after a prune pass that deleted at least one session.
    #[serde(default = "default_sessions_vacuum_after_prune")]
    pub vacuum_after_prune: bool,
    /// Minimum interval between maintenance passes.
    #[serde(default = "default_sessions_min_interval_hours")]
    pub min_interval_hours: u32,
}

impl Default for SessionsMaintenanceConfig {
    fn default() -> Self {
        Self {
            auto_prune: false,
            retention_days: default_sessions_retention_days(),
            vacuum_after_prune: default_sessions_vacuum_after_prune(),
            min_interval_hours: default_sessions_min_interval_hours(),
        }
    }
}

fn default_sessions_retention_days() -> u32 {
    90
}

fn default_sessions_vacuum_after_prune() -> bool {
    true
}

fn default_sessions_min_interval_hours() -> u32 {
    24
}

/// Default platform-to-toolset mapping, aligned with Python gateway defaults.
pub fn default_platform_toolsets() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    map.insert("cli".to_string(), vec!["hermes-cli".to_string()]);
    map.insert("telegram".to_string(), vec!["hermes-telegram".to_string()]);
    map.insert("discord".to_string(), vec!["hermes-discord".to_string()]);
    map.insert("whatsapp".to_string(), vec!["hermes-whatsapp".to_string()]);
    map.insert("slack".to_string(), vec!["hermes-slack".to_string()]);
    map
}

// ---------------------------------------------------------------------------
// AgentLoopBehaviorConfig (Python-shaped nudge + background review)
// ---------------------------------------------------------------------------

/// Mirrors Python defaults: `memory.nudge_interval` / `skills.creation_nudge_interval`,
/// and implicit background memory/skill review when those intervals fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoopBehaviorConfig {
    #[serde(default = "default_agent_memory_nudge_interval")]
    pub memory_nudge_interval: u32,
    #[serde(default = "default_agent_skill_nudge_interval")]
    pub skill_creation_nudge_interval: u32,
    /// Skip auto-injected workspace/personal context files (SOUL.md, AGENTS.md, etc.).
    /// Useful for batch-style runs where personalized instructions would pollute trajectories.
    #[serde(default = "default_agent_skip_context_files")]
    pub skip_context_files: bool,
    /// When true (default), spawn the extra LLM pass for memory/skill review — Python has no master off-switch.
    #[serde(default = "default_agent_background_review_enabled")]
    pub background_review_enabled: bool,
    /// Enable always-on workspace code indexing + repo-map context injection.
    #[serde(default = "default_agent_code_index_enabled")]
    pub code_index_enabled: bool,
    /// Maximum files included in repo-map prompt block.
    #[serde(default = "default_agent_code_index_max_files")]
    pub code_index_max_files: usize,
    /// Maximum symbols included in repo-map prompt block.
    #[serde(default = "default_agent_code_index_max_symbols")]
    pub code_index_max_symbols: usize,
    /// Enable LSP-style context injection after file operations.
    #[serde(default = "default_agent_lsp_context_enabled")]
    pub lsp_context_enabled: bool,
    /// Character budget for injected LSP context block.
    #[serde(default = "default_agent_lsp_context_max_chars")]
    pub lsp_context_max_chars: usize,
}

fn default_agent_memory_nudge_interval() -> u32 {
    10
}

fn default_agent_skill_nudge_interval() -> u32 {
    10
}

fn default_agent_skip_context_files() -> bool {
    false
}

fn default_agent_background_review_enabled() -> bool {
    true
}

fn default_agent_code_index_enabled() -> bool {
    true
}

fn default_agent_code_index_max_files() -> usize {
    32
}

fn default_agent_code_index_max_symbols() -> usize {
    160
}

fn default_agent_lsp_context_enabled() -> bool {
    true
}

fn default_agent_lsp_context_max_chars() -> usize {
    2_800
}

impl Default for AgentLoopBehaviorConfig {
    fn default() -> Self {
        Self {
            memory_nudge_interval: default_agent_memory_nudge_interval(),
            skill_creation_nudge_interval: default_agent_skill_nudge_interval(),
            skip_context_files: default_agent_skip_context_files(),
            background_review_enabled: default_agent_background_review_enabled(),
            code_index_enabled: default_agent_code_index_enabled(),
            code_index_max_files: default_agent_code_index_max_files(),
            code_index_max_symbols: default_agent_code_index_max_symbols(),
            lsp_context_enabled: default_agent_lsp_context_enabled(),
            lsp_context_max_chars: default_agent_lsp_context_max_chars(),
        }
    }
}

fn default_max_turns() -> u32 {
    250
}

fn default_tools() -> Vec<String> {
    vec![
        "bash".into(),
        "read".into(),
        "write".into(),
        "edit".into(),
        "glob".into(),
        "grep".into(),
        "web_search".into(),
        "web_fetch".into(),
    ]
}

// ---------------------------------------------------------------------------
// LlmProviderConfig
// ---------------------------------------------------------------------------

/// Configuration for a named LLM provider endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// API key (or env-var reference like "${MY_API_KEY}").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Environment variable name that stores the API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Base URL for the provider API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional external-process command used by runtime-provider resolvers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Optional external-process argv tail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Default model to use for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Maximum tokens in the completion response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Extra JSON body fields forwarded to the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,

    /// Requests-per-minute rate limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,

    /// Pool of credential identifiers for rotation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_pool: Vec<String>,

    /// OAuth2 token endpoint used for refresh flows (e.g. openai-codex, qwen-oauth).
    /// When unset, falls back to provider-specific `HERMES_<PROVIDER>_OAUTH_TOKEN_URL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_token_url: Option<String>,

    /// OAuth2 client_id used for refresh flows.
    /// When unset, falls back to provider-specific `HERMES_<PROVIDER>_OAUTH_CLIENT_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

impl Default for LlmProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            api_key_env: None,
            base_url: None,
            command: None,
            args: Vec::new(),
            model: None,
            max_tokens: None,
            temperature: None,
            extra_body: None,
            rate_limit: None,
            credential_pool: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// SmartModelRoutingConfig
// ---------------------------------------------------------------------------

/// Route short/simple turns to a cheaper model while preserving the primary model
/// for complex prompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartModelRoutingConfig {
    /// Master switch.
    #[serde(default)]
    pub enabled: bool,
    /// Max chars for a message to be considered "simple".
    #[serde(default = "default_max_simple_chars")]
    pub max_simple_chars: usize,
    /// Max words for a message to be considered "simple".
    #[serde(default = "default_max_simple_words")]
    pub max_simple_words: usize,
    /// Optional cheap route target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_model: Option<CheapModelRouteConfig>,
}

impl Default for SmartModelRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_simple_chars: default_max_simple_chars(),
            max_simple_words: default_max_simple_words(),
            cheap_model: None,
        }
    }
}

/// Cheap route target details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CheapModelRouteConfig {
    /// Optional provider; when set and `model` lacks provider prefix, runtime
    /// can compose `<provider>:<model>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model slug (required for routing to activate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional endpoint override (reserved for parity with Python config shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional env var name for api key (reserved for parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

fn default_max_simple_chars() -> usize {
    160
}

fn default_max_simple_words() -> usize {
    28
}

// ---------------------------------------------------------------------------
// TerminalConfig / TerminalBackendType
// ---------------------------------------------------------------------------

/// Which backend to use for terminal/command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalBackendType {
    Local,
    Docker,
    Ssh,
    Daytona,
    Modal,
    Singularity,
}

impl Default for TerminalBackendType {
    fn default() -> Self {
        Self::Local
    }
}

/// Configuration for terminal/command-execution backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Which backend type to use.
    #[serde(default)]
    pub backend: TerminalBackendType,

    /// Timeout in seconds for a single command.
    #[serde(default = "default_terminal_timeout")]
    pub timeout: u64,

    /// Maximum output size in bytes.
    #[serde(default = "default_max_output_size")]
    pub max_output_size: usize,

    /// Working directory override for command execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,

    /// Docker container id/name to reuse instead of creating a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_container_id: Option<String>,

    /// Docker image used when the Docker backend creates a container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,

    /// Mount the current host directory into Docker at `/workspace`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub docker_mount_cwd_to_workspace: bool,

    /// Runtime name for Vercel-backed terminal execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_runtime: Option<String>,

    /// SSH backend host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,

    /// SSH backend port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,

    /// SSH backend username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,

    /// SSH backend private-key path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            backend: TerminalBackendType::default(),
            timeout: default_terminal_timeout(),
            max_output_size: default_max_output_size(),
            workdir: None,
            docker_container_id: None,
            docker_image: None,
            docker_mount_cwd_to_workspace: false,
            vercel_runtime: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_key_path: None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_terminal_timeout() -> u64 {
    120
}

fn default_max_output_size() -> usize {
    1_048_576 // 1 MiB
}

// ---------------------------------------------------------------------------
// ApprovalConfig
// ---------------------------------------------------------------------------

/// Approval / safety-gate settings for dangerous operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalConfig {
    /// Whether the approval gate is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// List of command patterns that are considered dangerous.
    #[serde(default)]
    pub dangerous_commands: Vec<String>,

    /// Whether to require explicit approval for all tool calls.
    #[serde(default)]
    pub require_approval: bool,

    /// Commands matching whitelist bypass confirmation.
    #[serde(default)]
    pub whitelist_commands: Vec<String>,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dangerous_commands: Vec::new(),
            require_approval: false,
            whitelist_commands: Vec::new(),
        }
    }
}

/// Security toggles aligned with Python config shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Allow private/internal URL resolution globally.
    ///
    /// This is intended for constrained network environments (for example
    /// TUN-mode proxies or split-tunnel VPNs) where public hosts resolve to
    /// RFC1918/CGNAT/benchmark ranges.
    #[serde(default)]
    pub allow_private_urls: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allow_private_urls: false,
        }
    }
}

/// Skills configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SkillsSettings {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

/// Tools configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolsSettings {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub per_tool: HashMap<String, serde_json::Value>,
}

/// MCP server entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether this MCP server supports parallel tool calls safely.
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
}

/// Active profile info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub available: Vec<String>,
}

// ---------------------------------------------------------------------------
// ProxyConfig
// ---------------------------------------------------------------------------

/// HTTP/SOCKS proxy settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// HTTP proxy URL (e.g. "http://proxy:8080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,

    /// SOCKS5 proxy URL (e.g. "socks5://proxy:1080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks_proxy: Option<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            http_proxy: None,
            socks_proxy: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.max_turns, 250);
        assert!(!cfg.tools.is_empty());
        assert!(cfg.model.is_none());
        assert!(cfg.auxiliary.is_empty());
        assert!(cfg.tts.is_null());
        assert!(cfg.proxy.is_none());
        assert_eq!(
            cfg.platform_toolsets
                .get("cli")
                .cloned()
                .unwrap_or_default(),
            vec!["hermes-cli".to_string()]
        );
        assert_eq!(
            cfg.platform_toolsets
                .get("telegram")
                .cloned()
                .unwrap_or_default(),
            vec!["hermes-telegram".to_string()]
        );
    }

    #[test]
    fn gateway_config_serde_roundtrip() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "vision".to_string(),
            AuxiliaryTaskConfig {
                provider: "openrouter".to_string(),
                model: "google/gemini-2.5-flash".to_string(),
                ..Default::default()
            },
        );
        cfg.tts = serde_json::json!({
            "provider": "piper",
            "piper": {"voice": "en_US-lessac-medium"}
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_turns, cfg.max_turns);
        assert_eq!(back.tools, cfg.tools);
        assert_eq!(back.auxiliary["vision"].model, "google/gemini-2.5-flash");
        assert_eq!(back.tts["provider"], "piper");
    }

    #[test]
    fn default_auxiliary_task_configs_match_upstream_shape() {
        let tasks = default_auxiliary_task_configs();
        for key in ["vision", "web_extract", "approval"] {
            let task = tasks.get(key).expect("built-in task default");
            assert_eq!(task.provider, "auto");
            assert_eq!(task.model, "");
            assert_eq!(task.base_url, "");
            assert_eq!(task.api_key, "");
        }
        assert_eq!(tasks["vision"].timeout, Some(120));
        assert_eq!(tasks["vision"].download_timeout, Some(30));
        assert_eq!(tasks["web_extract"].timeout, Some(360));
        assert_eq!(tasks["curator"].timeout, Some(600));
    }

    #[test]
    fn builtin_auxiliary_env_overrides_bridge_non_default_values() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "vision".to_string(),
            AuxiliaryTaskConfig {
                provider: "  openrouter  ".to_string(),
                model: "  google/gemini-2.5-flash  ".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "web_extract".to_string(),
            AuxiliaryTaskConfig {
                provider: "auto".to_string(),
                model: "custom-llm".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "approval".to_string(),
            AuxiliaryTaskConfig {
                base_url: "http://localhost:1234/v1".to_string(),
                api_key: "local-key".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(
            cfg.builtin_auxiliary_env_overrides(),
            vec![
                (
                    "AUXILIARY_APPROVAL_BASE_URL".to_string(),
                    "http://localhost:1234/v1".to_string()
                ),
                (
                    "AUXILIARY_APPROVAL_API_KEY".to_string(),
                    "local-key".to_string()
                ),
                (
                    "AUXILIARY_VISION_PROVIDER".to_string(),
                    "openrouter".to_string()
                ),
                (
                    "AUXILIARY_VISION_MODEL".to_string(),
                    "google/gemini-2.5-flash".to_string()
                ),
                (
                    "AUXILIARY_WEB_EXTRACT_MODEL".to_string(),
                    "custom-llm".to_string()
                ),
            ]
        );
    }

    #[test]
    fn auxiliary_env_overrides_skip_compression_until_registered() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "compression".to_string(),
            AuxiliaryTaskConfig {
                provider: "openrouter".to_string(),
                model: "compressor".to_string(),
                ..Default::default()
            },
        );

        assert!(cfg.builtin_auxiliary_env_overrides().is_empty());
        assert_eq!(
            cfg.auxiliary_env_overrides_for(["compression"]),
            vec![
                (
                    "AUXILIARY_COMPRESSION_PROVIDER".to_string(),
                    "openrouter".to_string()
                ),
                (
                    "AUXILIARY_COMPRESSION_MODEL".to_string(),
                    "compressor".to_string()
                ),
            ]
        );
    }

    #[test]
    fn quick_commands_deserialize_exec_and_alias_configs() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
quick_commands:
  dn:
    type: exec
    command: echo daily-note
    timeout_secs: 5
  sc:
    type: alias
    target: /context
"#,
        )
        .expect("quick command config");

        let exec = cfg.quick_commands.get("dn").expect("exec command");
        assert_eq!(exec.kind, "exec");
        assert_eq!(exec.command.as_deref(), Some("echo daily-note"));
        assert_eq!(exec.timeout_secs(), 5);

        let alias = cfg.quick_commands.get("sc").expect("alias command");
        assert_eq!(alias.kind, "alias");
        assert_eq!(alias.target.as_deref(), Some("/context"));
    }

    #[test]
    fn terminal_backend_type_serde() {
        let t = TerminalBackendType::Docker;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"docker\"");
        let back: TerminalBackendType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TerminalBackendType::Docker);
    }

    #[test]
    fn approval_config_default() {
        let a = ApprovalConfig::default();
        assert!(!a.enabled);
        assert!(!a.require_approval);
        assert!(a.dangerous_commands.is_empty());
    }

    #[test]
    fn security_config_default() {
        let s = SecurityConfig::default();
        assert!(!s.allow_private_urls);
    }

    #[test]
    fn proxy_config_serde() {
        let p = ProxyConfig {
            http_proxy: Some("http://proxy:8080".into()),
            socks_proxy: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.http_proxy, Some("http://proxy:8080".to_string()));
        assert_eq!(back.socks_proxy, None);
    }
}
