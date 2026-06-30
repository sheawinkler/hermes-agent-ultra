//! MCP Client (Requirement 14.1-14.3)
//!
//! Connects to external MCP servers, discovers their tools, and
//! dispatches tool calls through the MCP protocol. When a server
//! sends `notifications/tools/list_changed`, the client automatically
//! rediscovers tools and updates the registry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hermes_core::{
    normalize_schema_definitions_refs, JsonSchema, ToolError, ToolHandler, ToolSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use hermes_tools::ToolRegistry;

use crate::auth::McpAuthProvider;
use crate::transport::{HttpSseTransport, McpTransport, StdioTransport};
use crate::McpError;

// ---------------------------------------------------------------------------
// ResourceInfo
// ---------------------------------------------------------------------------

/// Information about a resource exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceInfo {
    /// URI identifying this resource (e.g. "file:///path/to/data").
    pub uri: String,
    /// Human-readable name of the resource.
    pub name: String,
    /// Optional description of what this resource contains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type of the resource content (e.g. "text/plain", "application/json").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Sampling types
// ---------------------------------------------------------------------------

/// Configuration for MCP sampling (server-initiated LLM requests).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SamplingConfig {
    #[serde(default = "default_sampling_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub max_rpm: u32,
    pub max_tokens_cap: u32,
    pub timeout_secs: u64,
    pub allowed_models: Vec<String>,
    pub max_tool_rounds: u32,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            max_rpm: 10,
            max_tokens_cap: 4096,
            timeout_secs: 60,
            allowed_models: vec![],
            max_tool_rounds: 3,
        }
    }
}

fn default_sampling_enabled() -> bool {
    true
}

/// Per-client sampling audit counters.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SamplingMetrics {
    pub requests: u64,
    pub errors: u64,
    pub tokens_used: u64,
    pub tool_use_count: u64,
    pub rate_limited: u64,
}

/// Callback type for LLM invocations triggered by MCP sampling.
pub type LlmCallback = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send>> + Send + Sync,
>;

const DEFAULT_MCP_CALL_TIMEOUT_SECS: u64 = 300;
const MAX_MCP_CALL_TIMEOUT_SECS: u64 = 900;
const DEFAULT_MCP_KEEPALIVE_INTERVAL_SECS: u64 = 180;
const MIN_MCP_KEEPALIVE_INTERVAL_SECS: u64 = 5;
const MCP_KEEPALIVE_PROBE_TIMEOUT_SECS: u64 = 30;
const STALE_TRANSPORT_MARKERS: &[&str] = &[
    "closedresourceerror",
    "closed resource",
    "transport is closed",
    "connection closed",
    "broken pipe",
    "end of file",
    "eof",
];
const MCP_SHELL_INTERPRETERS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "dash",
    "fish",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
];
const MCP_EGRESS_COMMANDS: &[&str] = &["curl", "wget", "nc", "ncat", "socat"];

// ---------------------------------------------------------------------------
// Prompt types
// ---------------------------------------------------------------------------

/// Information about a prompt exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

/// A single argument descriptor for an MCP prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Result of getting a prompt from an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

/// A single message in a prompt result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptsListResponse {
    pub prompts: Vec<PromptInfo>,
}

// ---------------------------------------------------------------------------
// Status / probe types
// ---------------------------------------------------------------------------

/// Status of a single MCP server connection.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tool_count: usize,
    pub resource_count: usize,
    pub transport_type: String,
    pub uptime_secs: Option<u64>,
}

/// Result from probing an MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpProbeResult {
    pub reachable: bool,
    pub latency_ms: u64,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub server_info: Option<Value>,
}

/// Per-server outcome from parallel MCP discovery.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpDiscoveryReport {
    pub name: String,
    pub connected: bool,
    pub transport_type: String,
    pub tool_count: usize,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// McpServerConfig
// ---------------------------------------------------------------------------

/// Configuration for connecting to an MCP server.
///
/// Supports two connection modes:
/// - **stdio**: Launch a local process and communicate via stdin/stdout (JSON-RPC)
/// - **HTTP**: Connect to a remote MCP server via HTTP/SSE
#[derive(Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to execute for stdio-based servers (e.g. "npx", "python").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments for the stdio command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// URL for remote (HTTP/SSE) MCP servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether this server supports concurrent tool calls from one session.
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
    /// Optional liveness probe cadence for HTTP/SSE sessions, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_interval: Option<u64>,
    /// Optional sampling policy for server-initiated LLM requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingConfig>,
    /// Optional authentication provider for remote servers.
    #[serde(skip)]
    pub auth_provider: Option<Arc<dyn McpAuthProvider>>,
    /// Runtime-only guard used by background discovery to prevent OAuth flows
    /// from competing with the interactive TUI/CLI stdin reader.
    #[serde(skip)]
    pub suppress_interactive_oauth: bool,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("url", &self.url)
            .field(
                "supports_parallel_tool_calls",
                &self.supports_parallel_tool_calls,
            )
            .field("keepalive_interval", &self.keepalive_interval)
            .field("sampling", &self.sampling)
            .field(
                "suppress_interactive_oauth",
                &self.suppress_interactive_oauth,
            )
            .field(
                "auth_provider",
                &self.auth_provider.as_ref().map(|_| "<McpAuthProvider>"),
            )
            .finish()
    }
}

impl PartialEq for McpServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
            && self.args == other.args
            && self.env == other.env
            && self.url == other.url
            && self.supports_parallel_tool_calls == other.supports_parallel_tool_calls
            && self.keepalive_interval == other.keepalive_interval
            && self.sampling == other.sampling
            && self.suppress_interactive_oauth == other.suppress_interactive_oauth
    }
}

impl McpServerConfig {
    /// Create a stdio-based config (local process).
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: Some(command.into()),
            args,
            env: HashMap::new(),
            url: None,
            supports_parallel_tool_calls: false,
            keepalive_interval: None,
            sampling: None,
            auth_provider: None,
            suppress_interactive_oauth: false,
        }
    }

    /// Create an HTTP-based config (remote server).
    pub fn http(url: impl Into<String>) -> Self {
        Self {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url.into()),
            supports_parallel_tool_calls: false,
            keepalive_interval: None,
            sampling: None,
            auth_provider: None,
            suppress_interactive_oauth: false,
        }
    }

    /// Set explicit parallel-tool-call capability for this server.
    pub fn with_parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.supports_parallel_tool_calls = enabled;
        self
    }

    /// Set the HTTP/SSE session keepalive cadence in seconds.
    pub fn with_keepalive_interval(mut self, seconds: u64) -> Self {
        self.keepalive_interval = Some(seconds);
        self
    }

    /// Set sampling policy for server-initiated LLM requests.
    pub fn with_sampling_config(mut self, config: SamplingConfig) -> Self {
        self.sampling = Some(config);
        self
    }

    /// Add environment variables to the config.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set an authentication provider for remote servers.
    pub fn with_auth(mut self, provider: Arc<dyn McpAuthProvider>) -> Self {
        self.auth_provider = Some(provider);
        self
    }

    /// Mark this config as running in background discovery, where OAuth must
    /// fail soft instead of prompting on stdin.
    pub fn with_interactive_oauth_suppressed(mut self) -> Self {
        self.suppress_interactive_oauth = true;
        self
    }

    /// Returns true if this config is for a stdio (local process) connection.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    /// Returns true if this config is for an HTTP (remote) connection.
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }
}

/// Return security warnings for a configured MCP server.
///
/// Stdio MCP intentionally supports arbitrary local commands. This guard only
/// blocks the high-signal exfiltration shape: a shell interpreter with inline
/// args that invoke network egress tooling.
pub fn validate_mcp_server_config(name: &str, config: &McpServerConfig) -> Vec<String> {
    let Some(command) = config.command.as_deref() else {
        return Vec::new();
    };
    let basename = command_basename(command);
    if !MCP_SHELL_INTERPRETERS.contains(&basename.as_str()) {
        return Vec::new();
    }

    let script = inline_stdio_script(&config.args);
    if script.trim().is_empty() || !contains_network_egress_shape(&script) {
        return Vec::new();
    }

    let mut issue = format!(
        "MCP server '{name}' uses shell interpreter '{command}' with network egress in args"
    );
    if contains_exfil_hint(&script) {
        issue.push_str(" and exfiltration-shaped arguments");
    }
    vec![issue]
}

pub fn is_mcp_server_config_suspicious(name: &str, config: &McpServerConfig) -> bool {
    !validate_mcp_server_config(name, config).is_empty()
}

fn command_basename(command: &str) -> String {
    let text = command.trim();
    if text.is_empty() {
        return String::new();
    }
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or(text)
        .trim_matches(|ch| ch == '"' || ch == '\'');
    Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase()
}

fn inline_stdio_script(args: &[String]) -> String {
    args.join(" ")
}

fn contains_network_egress_shape(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    MCP_EGRESS_COMMANDS
        .iter()
        .any(|command| contains_bounded_token(&lower, command))
        || lower.contains("/dev/tcp/")
        || lower.contains("invoke-webrequest")
        || lower.contains("invoke-restmethod")
        || lower.contains("system.net.webclient")
}

fn contains_exfil_hint(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    lower.contains(".env")
        || lower.contains("--data-binary")
        || lower.contains("--data-raw")
        || lower.contains("-x post")
        || contains_bounded_token(&lower, "post")
        || contains_file_redirection(&lower)
}

fn contains_bounded_token(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(idx, _)| {
        let before = haystack[..idx].chars().next_back();
        let after = haystack[idx + needle.len()..].chars().next();
        !is_word_dot_hyphen(before) && !is_word_dot_hyphen(after)
    })
}

fn is_word_dot_hyphen(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-')
}

fn contains_file_redirection(script: &str) -> bool {
    script
        .split('<')
        .skip(1)
        .any(|tail| tail.chars().any(|ch| !ch.is_whitespace()))
}

fn transport_type_for_config(config: &McpServerConfig) -> &'static str {
    if config.is_http() {
        "http"
    } else if config.is_stdio() {
        "stdio"
    } else {
        "unknown"
    }
}

/// Return an MCP server/tool name component safe for provider tool names.
///
/// This mirrors the Python adapter's rule: every character outside
/// `[A-Za-z0-9_]` becomes `_`.
pub fn sanitize_mcp_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn mcp_toolset_name(server_name: &str) -> String {
    format!("mcp-{}", sanitize_mcp_name_component(server_name))
}

fn mcp_registered_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp_{}_{}",
        sanitize_mcp_name_component(server_name),
        sanitize_mcp_name_component(tool_name)
    )
}

// ---------------------------------------------------------------------------
// MCP protocol types (deserialization helpers)
// ---------------------------------------------------------------------------

/// Result from the MCP initialize method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: Value,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// Server info returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// MCP tool definition from the protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct McpToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Response from tools/list method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ToolsListResponse {
    pub tools: Vec<McpToolDefinition>,
}

fn mcp_input_schema_to_json_schema(input_schema: Value) -> JsonSchema {
    let normalized = normalize_schema_definitions_refs(&input_schema);
    serde_json::from_value(normalized).unwrap_or_else(|_| JsonSchema::new("object"))
}

/// Response from resources/list method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ResourcesListResponse {
    pub resources: Vec<ResourceInfo>,
}

fn mcp_call_timeout_duration() -> Duration {
    let secs = std::env::var("HERMES_MCP_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MCP_CALL_TIMEOUT_SECS)
        .min(MAX_MCP_CALL_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn is_stale_transport_error(err: &McpError) -> bool {
    if matches!(err, McpError::ConnectionClosed) {
        return true;
    }
    let message = match err {
        McpError::ConnectionError(m) => m,
        McpError::Protocol { message, .. } => message,
        McpError::Io(m) => m,
        _ => return false,
    };
    let lower = message.to_ascii_lowercase();
    STALE_TRANSPORT_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn mcp_home_dir() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_HOME") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".hermes-agent-ultra");
        }
    }
    PathBuf::from(".hermes-agent-ultra")
}

fn mcp_image_cache_dir() -> PathBuf {
    mcp_home_dir().join("cache").join("images")
}

fn mcp_image_extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        "image/svg+xml" => ".svg",
        _ => ".png",
    }
}

fn looks_like_image_bytes(data: &[u8]) -> bool {
    data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) // PNG
        || data.starts_with(&[0xFF, 0xD8, 0xFF]) // JPEG
        || data.starts_with(b"GIF87a")
        || data.starts_with(b"GIF89a")
        || (data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP")
        || data.starts_with(b"BM")
}

fn cache_mcp_image_block(item: &Value) -> Option<String> {
    let typ = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !typ.eq_ignore_ascii_case("image") && !typ.eq_ignore_ascii_case("image_content") {
        return None;
    }
    let data_b64 = item.get("data").and_then(|v| v.as_str())?;
    let mime_type = item
        .get("mimeType")
        .or_else(|| item.get("mime_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("image/png")
        .trim();
    if !mime_type.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
        Ok(b) => b,
        Err(e) => {
            warn!("MCP image block decode failed ({}): {}", mime_type, e);
            return None;
        }
    };
    if !looks_like_image_bytes(&bytes) {
        warn!(
            "MCP image block rejected by signature check ({})",
            mime_type
        );
        return None;
    }
    let cache_dir = mcp_image_cache_dir();
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        warn!(
            "MCP image cache mkdir failed ({}): {}",
            cache_dir.display(),
            e
        );
        return None;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ext = mcp_image_extension_for_mime_type(mime_type);
    let file_name = format!("mcp-image-{}-{}{}", std::process::id(), ts, ext);
    let file_path = cache_dir.join(file_name);
    if let Err(e) = std::fs::write(&file_path, &bytes) {
        warn!(
            "MCP image cache write failed ({}): {}",
            file_path.display(),
            e
        );
        return None;
    }
    Some(format!("MEDIA:{}", file_path.display()))
}

fn extract_mcp_error_message(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        for item in content {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    if let Some(message) = result.get("message").and_then(|m| m.as_str()) {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(kind) = result
        .get("errorType")
        .or_else(|| result.get("error_type"))
        .and_then(|v| v.as_str())
    {
        let trimmed = kind.trim();
        if !trimmed.is_empty() {
            return format!("{trimmed} (empty error message)");
        }
    }
    "tool call returned error".to_string()
}

// ---------------------------------------------------------------------------
// McpClient — single-server connection
// ---------------------------------------------------------------------------

/// A client connection to a single MCP server.
///
/// Handles the full lifecycle of communicating with one MCP server:
/// connecting, initializing, discovering tools, calling tools, reading
/// resources, and disconnecting.
pub struct McpClient {
    /// The configuration used to establish this connection.
    pub config: McpServerConfig,
    /// The transport layer for this connection.
    transport: Option<Box<dyn McpTransport>>,
    /// Cached list of tools discovered from this server.
    tools: Vec<ToolSchema>,
    /// Cached list of resources discovered from this server.
    resources: Vec<ResourceInfo>,
    /// JSON-RPC request ID counter.
    next_id: u64,
    /// Whether the connection has been initialized.
    connected: bool,
    /// Sampling configuration for server-initiated LLM requests.
    sampling_config: Option<SamplingConfig>,
    /// LLM callback used for transport-loop sampling/createMessage requests.
    sampling_callback: Option<LlmCallback>,
    /// Sliding-window timestamps for sampling rate limiting.
    sampling_rate_timestamps: VecDeque<Instant>,
    /// Consecutive sampling tool-use response count.
    sampling_tool_rounds: u32,
    /// Per-client sampling audit counters.
    sampling_metrics: SamplingMetrics,
    /// Timestamp when the client connected (for uptime tracking).
    connected_at: Option<Instant>,
    /// Latched when a server returns JSON-RPC -32601 for optional `ping`.
    ping_unsupported: bool,
}

include!("client/client_impl.rs");

include!("client/manager.rs");

#[cfg(test)]
mod tests;
