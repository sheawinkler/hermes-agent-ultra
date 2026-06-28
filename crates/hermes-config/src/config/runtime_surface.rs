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

impl TerminalBackendType {
    pub fn from_env_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "docker" => Some(Self::Docker),
            "ssh" => Some(Self::Ssh),
            "daytona" => Some(Self::Daytona),
            "modal" => Some(Self::Modal),
            "singularity" | "apptainer" => Some(Self::Singularity),
            _ => None,
        }
    }
}

/// HOME policy for host subprocesses spawned by terminal/code tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalHomeMode {
    /// Keep host subprocesses on the real OS-user HOME; container runtimes may
    /// still use profile-scoped HOME when they own a persistent data volume.
    Auto,
    /// Force subprocesses to the real OS-user HOME.
    Real,
    /// Force subprocesses to `$HERMES_HOME/home`.
    Profile,
}

impl Default for TerminalHomeMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl TerminalHomeMode {
    pub fn from_env_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "real" => Some(Self::Real),
            "profile" | "isolated" => Some(Self::Profile),
            _ => None,
        }
    }

    pub fn as_env_name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Real => "real",
            Self::Profile => "profile",
        }
    }
}

/// Configuration for terminal/command-execution backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// Which backend type to use.
    #[serde(default, alias = "env_type")]
    pub backend: TerminalBackendType,

    /// Timeout in seconds for a single command.
    #[serde(default = "default_terminal_timeout")]
    pub timeout: u64,

    /// Maximum output size in bytes.
    #[serde(default = "default_max_output_size")]
    pub max_output_size: usize,

    /// Working directory override for command execution.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "cwd")]
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

    /// Run Docker containers as the host uid/gid where supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub docker_run_as_host_user: bool,

    /// Docker container CPU limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_cpu: Option<u32>,

    /// Docker/container memory limit in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_memory: Option<u64>,

    /// Docker/container disk limit in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_disk: Option<u64>,

    /// Whether container-backed terminal sessions should persist.
    #[serde(default, skip_serializing_if = "is_false")]
    pub container_persistent: bool,

    /// Extra env-vars for Docker/container execution, kept as a portable string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_env: Option<String>,

    /// Host env-var names to forward into Docker/container execution.
    #[serde(
        default,
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub docker_forward_env: Vec<String>,

    /// Extra Docker volume specs.
    #[serde(
        default,
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub docker_volumes: Vec<String>,

    /// Runtime name for Vercel-backed terminal execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_runtime: Option<String>,

    /// Modal backend selection mode: auto, direct, or managed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal_mode: Option<String>,

    /// Explicit shell init files to source before local commands.
    #[serde(
        default,
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub shell_init_files: Vec<String>,

    /// Auto-source common shell startup files when no explicit list is set.
    #[serde(
        default = "default_auto_source_bashrc",
        skip_serializing_if = "is_true"
    )]
    pub auto_source_bashrc: bool,

    /// HOME policy for host subprocesses spawned by terminal/code tools.
    #[serde(default, skip_serializing_if = "is_default_home_mode")]
    pub home_mode: TerminalHomeMode,

    /// Host env-var names allowed through provider/tool subprocess sanitizers.
    #[serde(
        default,
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub env_passthrough: Vec<String>,

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
            docker_run_as_host_user: false,
            container_cpu: None,
            container_memory: None,
            container_disk: None,
            container_persistent: false,
            docker_env: None,
            docker_forward_env: Vec::new(),
            docker_volumes: Vec::new(),
            vercel_runtime: None,
            modal_mode: None,
            shell_init_files: Vec::new(),
            auto_source_bashrc: default_auto_source_bashrc(),
            home_mode: TerminalHomeMode::default(),
            env_passthrough: Vec::new(),
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

fn is_true(value: &bool) -> bool {
    *value
}

fn is_default_home_mode(value: &TerminalHomeMode) -> bool {
    *value == TerminalHomeMode::default()
}

fn default_true() -> bool {
    true
}

fn default_terminal_timeout() -> u64 {
    120
}

fn default_auto_source_bashrc() -> bool {
    true
}

fn default_max_output_size() -> usize {
    1_048_576 // 1 MiB
}

// ---------------------------------------------------------------------------
// WebConfig
// ---------------------------------------------------------------------------

/// Web backend selection knobs aligned with Python config shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WebConfig {
    /// Shared legacy backend selector.
    #[serde(
        default,
        deserialize_with = "deserialize_string_or_empty",
        skip_serializing_if = "String::is_empty"
    )]
    pub backend: String,

    /// Search-specific backend selector.
    #[serde(
        default,
        deserialize_with = "deserialize_string_or_empty",
        skip_serializing_if = "String::is_empty"
    )]
    pub search_backend: String,

    /// Extract-specific backend selector.
    #[serde(
        default,
        deserialize_with = "deserialize_string_or_empty",
        skip_serializing_if = "String::is_empty"
    )]
    pub extract_backend: String,

    /// Crawl-specific backend selector.
    #[serde(
        default,
        deserialize_with = "deserialize_string_or_empty",
        skip_serializing_if = "String::is_empty"
    )]
    pub crawl_backend: String,
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

    /// Website/domain blocklist used by web-facing tools.
    #[serde(default)]
    pub website_blocklist: WebsiteBlocklistConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allow_private_urls: false,
            website_blocklist: WebsiteBlocklistConfig::default(),
        }
    }
}

/// Website/domain blocklist configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WebsiteBlocklistConfig {
    /// Enable domain blocklist enforcement.
    #[serde(default)]
    pub enabled: bool,

    /// Inline blocked domains or wildcard domain patterns.
    #[serde(default)]
    pub domains: Vec<String>,

    /// Additional newline-delimited blocklist files.
    #[serde(default)]
    pub shared_files: Vec<String>,
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
    /// Optional HTTP/SSE session keepalive cadence in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_interval: Option<u64>,
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
