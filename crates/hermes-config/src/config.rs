//! Gateway configuration: the top-level config struct and its sub-types.

use std::collections::HashMap;

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

    /// Optional per-turn smart model routing (cheap-vs-strong).
    #[serde(default)]
    pub smart_model_routing: SmartModelRoutingConfig,

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
            smart_model_routing: SmartModelRoutingConfig::default(),
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
    30
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
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            backend: TerminalBackendType::default(),
            timeout: default_terminal_timeout(),
            max_output_size: default_max_output_size(),
            workdir: None,
        }
    }
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
        assert_eq!(cfg.max_turns, 30);
        assert!(!cfg.tools.is_empty());
        assert!(cfg.model.is_none());
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
        let cfg = GatewayConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_turns, cfg.max_turns);
        assert_eq!(back.tools, cfg.tools);
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
