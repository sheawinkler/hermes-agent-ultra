//! Gateway configuration: the top-level config struct and its sub-types.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

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
    /// Default LLM model identifier (for example `nous:nousresearch/hermes-4-70b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Model-switch persistence controls.
    #[serde(default)]
    pub model_switch: ModelSwitchConfig,

    /// Personality / persona name to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Maximum agent conversation turns before forced stop.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Custom system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Optional JSON few-shot/prefill message file.
    ///
    /// This is the canonical upstream-compatible key. Messages are injected at
    /// runtime only and must not be persisted into session history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill_messages_file: Option<String>,

    /// List of enabled tool names. Defaults to all core tools.
    #[serde(default = "default_tools")]
    pub tools: Vec<String>,

    /// Budget limits for tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Configurable raw tool-output truncation limits.
    #[serde(default)]
    pub tool_output: ToolOutputConfig,

    /// Per-platform configuration (keyed by platform name, e.g. "discord").
    #[serde(default)]
    pub platforms: HashMap<String, PlatformConfig>,

    /// Per-platform toolset selection (e.g. cli/hermes-cli, telegram/hermes-telegram).
    #[serde(default = "default_platform_toolsets")]
    pub platform_toolsets: HashMap<String, Vec<String>>,

    /// Sub-agent delegation controls.
    #[serde(default)]
    pub delegation: DelegationConfig,

    /// Session management settings.
    #[serde(default)]
    pub session: SessionConfig,

    /// Session persistence database maintenance (auto-prune + VACUUM).
    #[serde(default)]
    pub sessions: SessionsMaintenanceConfig,

    /// Streaming / progressive-output settings.
    #[serde(default)]
    pub streaming: StreamingConfig,

    /// Display controls shared by CLI/gateway surfaces.
    #[serde(default)]
    pub display: DisplayConfig,

    /// Terminal / command-execution backend settings.
    #[serde(default)]
    pub terminal: TerminalConfig,

    /// Web search/extract/crawl backend selection.
    #[serde(default)]
    pub web: WebConfig,

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

    /// Kanban dispatcher/notifier behavior for multi-gateway deployments.
    #[serde(default)]
    pub kanban: KanbanConfig,

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
            model_switch: ModelSwitchConfig::default(),
            personality: None,
            max_turns: default_max_turns(),
            system_prompt: None,
            prefill_messages_file: None,
            tools: default_tools(),
            budget: BudgetConfig::default(),
            tool_output: ToolOutputConfig::default(),
            platforms: HashMap::new(),
            platform_toolsets: default_platform_toolsets(),
            delegation: DelegationConfig::default(),
            session: SessionConfig::default(),
            sessions: SessionsMaintenanceConfig::default(),
            streaming: StreamingConfig::default(),
            display: DisplayConfig::default(),
            terminal: TerminalConfig::default(),
            web: WebConfig::default(),
            llm_providers: HashMap::new(),
            fallback_model: None,
            fallback_models: Vec::new(),
            smart_model_routing: SmartModelRoutingConfig::default(),
            auxiliary: BTreeMap::new(),
            quick_commands: BTreeMap::new(),
            kanban: KanbanConfig::default(),
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
pub struct ModelSwitchConfig {
    /// Persist plain `/model <name>` switches to config by default.
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_boolish",
        skip_serializing_if = "is_true"
    )]
    pub persist_switch_by_default: bool,
}

impl Default for ModelSwitchConfig {
    fn default() -> Self {
        Self {
            persist_switch_by_default: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DelegationConfig {
    /// Maximum sub-agent spawn depth. Values below 1 are floored by runtime consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    /// Optional model override for child agents. When set without a provider,
    /// children keep the parent's provider/credentials and only switch model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional provider override for child agents. Runtime consumers resolve
    /// the provider's full credential bundle before spawning the child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional direct endpoint override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional direct API key override for delegated child agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanConfig {
    /// When false, this process must not own Kanban dispatch/notifier polling.
    #[serde(default = "default_true")]
    pub dispatch_in_gateway: bool,
}

impl Default for KanbanConfig {
    fn default() -> Self {
        Self {
            dispatch_in_gateway: true,
        }
    }
}

pub const DEFAULT_TOOL_OUTPUT_MAX_BYTES: usize = 50_000;
pub const DEFAULT_TOOL_OUTPUT_MAX_LINES: usize = 2_000;
pub const DEFAULT_TOOL_OUTPUT_MAX_LINE_LENGTH: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolOutputConfig {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_line_length: usize,
}

impl Default for ToolOutputConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_TOOL_OUTPUT_MAX_BYTES,
            max_lines: DEFAULT_TOOL_OUTPUT_MAX_LINES,
            max_line_length: DEFAULT_TOOL_OUTPUT_MAX_LINE_LENGTH,
        }
    }
}

impl<'de> Deserialize<'de> for ToolOutputConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::from_value(&value))
    }
}

impl ToolOutputConfig {
    pub fn from_value(value: &serde_json::Value) -> Self {
        let mut config = Self::default();
        let Some(map) = value.as_object() else {
            return config;
        };

        if let Some(max_bytes) = map.get("max_bytes").and_then(Self::positive_usize) {
            config.max_bytes = max_bytes;
        }
        if let Some(max_lines) = map.get("max_lines").and_then(Self::positive_usize) {
            config.max_lines = max_lines;
        }
        if let Some(max_line_length) = map.get("max_line_length").and_then(Self::positive_usize) {
            config.max_line_length = max_line_length;
        }

        config
    }

    fn positive_usize(value: &serde_json::Value) -> Option<usize> {
        if let Some(raw) = value.as_u64() {
            return usize::try_from(raw).ok().filter(|v| *v > 0);
        }
        value
            .as_str()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Enable `/verbose` as a runtime tool-progress cycling command.
    #[serde(
        default,
        deserialize_with = "deserialize_boolish",
        skip_serializing_if = "is_false"
    )]
    pub tool_progress_command: bool,

    /// Global tool-progress display mode: `off`, `new`, `all`, or `verbose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_progress: Option<String>,

    /// Busy-session input mode for gateway surfaces: `interrupt`, `queue`, or `steer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_input_mode: Option<String>,

    /// Whether gateway busy-session guard should acknowledge queued/interrupted messages.
    #[serde(
        default,
        deserialize_with = "deserialize_option_boolish",
        skip_serializing_if = "Option::is_none"
    )]
    pub busy_ack_enabled: Option<bool>,

    /// Whether background memory/self-improvement summaries should be sent.
    #[serde(
        default,
        deserialize_with = "deserialize_option_boolish",
        skip_serializing_if = "Option::is_none"
    )]
    pub memory_notifications: Option<bool>,

    /// Per-platform display overrides keyed by normalized platform name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<String, PlatformDisplayConfig>,
}

impl DisplayConfig {
    pub fn tool_progress_command_enabled(&self) -> bool {
        self.tool_progress_command
    }

    pub fn platform_tool_progress(&self, platform: &str) -> Option<&str> {
        let key = platform.trim().to_ascii_lowercase().replace('-', "_");
        self.platforms
            .get(&key)
            .and_then(|cfg| cfg.tool_progress.as_deref())
            .or(self.tool_progress.as_deref())
    }

    pub fn normalized_busy_input_mode(&self) -> &'static str {
        match self
            .busy_input_mode
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "queue" | "queued" => "queue",
            "steer" | "steering" => "steer",
            "interrupt" | "interrupted" | "replace" | "" => "interrupt",
            _ => "interrupt",
        }
    }

    pub fn busy_ack_enabled(&self) -> bool {
        self.busy_ack_enabled.unwrap_or(true)
    }

    pub fn memory_notifications_enabled(&self) -> bool {
        self.memory_notifications.unwrap_or(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlatformDisplayConfig {
    /// Platform-specific tool-progress mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_progress: Option<String>,
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
    #[serde(
        default = "default_auxiliary_provider",
        deserialize_with = "deserialize_provider_or_default"
    )]
    pub provider: String,
    /// Empty means use the selected provider's default auxiliary model.
    #[serde(default, deserialize_with = "deserialize_string_or_empty")]
    pub model: String,
    /// Direct OpenAI-compatible endpoint. When set, it takes precedence over provider.
    #[serde(default, deserialize_with = "deserialize_string_or_empty")]
    pub base_url: String,
    /// API key for a direct endpoint or explicit task provider.
    #[serde(default, deserialize_with = "deserialize_string_or_empty")]
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

/// Auxiliary slot pinned to a provider that differs from the selected main provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleAuxiliaryAssignment {
    pub task: String,
    pub provider: String,
    pub model: String,
}

fn deserialize_string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(String::new()),
        Some(serde_json::Value::String(value)) => Ok(value),
        Some(value) => Err(D::Error::custom(format!(
            "expected string or null, got {value}"
        ))),
    }
}

fn deserialize_provider_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(default_auxiliary_provider()),
        Some(serde_json::Value::String(value)) => Ok(value),
        Some(value) => Err(D::Error::custom(format!(
            "expected provider string or null, got {value}"
        ))),
    }
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
    /// Return auxiliary tasks pinned to a provider other than `main_provider`.
    ///
    /// Switching the main model does not clear auxiliary overrides. This helper
    /// surfaces the silent mismatch so callers can warn and offer reset guidance
    /// without destroying legitimate dedicated auxiliary model choices.
    pub fn stale_auxiliary_assignments_for_main_provider(
        &self,
        main_provider: &str,
    ) -> Vec<StaleAuxiliaryAssignment> {
        let main_provider = main_provider.trim().to_ascii_lowercase();
        if main_provider.is_empty() {
            return Vec::new();
        }
        self.auxiliary
            .iter()
            .filter_map(|(task, cfg)| {
                let provider = cfg.provider.trim();
                if provider.is_empty()
                    || provider.eq_ignore_ascii_case("auto")
                    || provider.eq_ignore_ascii_case(&main_provider)
                {
                    return None;
                }
                Some(StaleAuxiliaryAssignment {
                    task: task.clone(),
                    provider: provider.to_string(),
                    model: cfg.model.trim().to_string(),
                })
            })
            .collect()
    }

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
    map.insert("tui".to_string(), vec!["hermes-cli".to_string()]);
    map.insert("desktop".to_string(), vec!["hermes-cli".to_string()]);
    map.insert("acp".to_string(), vec!["hermes-acp".to_string()]);
    map.insert("telegram".to_string(), vec!["hermes-telegram".to_string()]);
    map.insert("discord".to_string(), vec!["hermes-discord".to_string()]);
    map.insert("whatsapp".to_string(), vec!["hermes-whatsapp".to_string()]);
    map.insert("slack".to_string(), vec!["hermes-slack".to_string()]);
    map.insert("cron".to_string(), vec!["hermes-cron".to_string()]);
    map
}

include!("config/agent_loop_behavior.rs");
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

    /// Per-request timeout in seconds for this provider transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_seconds: Option<f64>,

    /// Optional external-process command used by runtime-provider resolvers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Optional external-process argv tail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Default model to use for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Explicit model allowlist for custom/named providers.
    #[serde(
        default,
        deserialize_with = "deserialize_provider_model_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub models: Vec<String>,

    /// Whether model pickers should live-probe `/models` for this provider.
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_boolish",
        skip_serializing_if = "is_true"
    )]
    pub discover_models: bool,

    /// Explicit provider wire protocol, e.g. `chat_completions` or `codex_responses`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_mode: Option<String>,

    /// Maximum tokens in the completion response.
    #[serde(
        default,
        alias = "max_output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
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
            request_timeout_seconds: None,
            command: None,
            args: Vec::new(),
            model: None,
            models: Vec::new(),
            discover_models: true,
            api_mode: None,
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

include!("config/runtime_surface.rs");
include!("config/tests.rs");
