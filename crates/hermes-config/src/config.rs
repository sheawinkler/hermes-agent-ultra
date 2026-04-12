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

    /// Session management settings.
    #[serde(default)]
    pub session: SessionConfig,

    /// Streaming / progressive-output settings.
    #[serde(default)]
    pub streaming: StreamingConfig,

    /// Terminal / command-execution backend settings.
    #[serde(default)]
    pub terminal: TerminalConfig,

    /// Named LLM provider configurations.
    #[serde(default)]
    pub llm_providers: HashMap<String, LlmProviderConfig>,

    /// Optional HTTP/SOCKS proxy settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Approval / safety-gate settings.
    #[serde(default)]
    pub approval: ApprovalConfig,

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
            session: SessionConfig::default(),
            streaming: StreamingConfig::default(),
            terminal: TerminalConfig::default(),
            llm_providers: HashMap::new(),
            proxy: None,
            approval: ApprovalConfig::default(),
            skills: SkillsSettings::default(),
            tools_config: ToolsSettings::default(),
            mcp_servers: Vec::new(),
            profile: ProfileConfig::default(),
            home_dir: None,
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

    /// Base URL for the provider API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

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
}

impl Default for LlmProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            model: None,
            max_tokens: None,
            temperature: None,
            extra_body: None,
            rate_limit: None,
            credential_pool: Vec::new(),
        }
    }
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
