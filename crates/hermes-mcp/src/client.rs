//! MCP Client (Requirement 14.1-14.3)
//!
//! Connects to external MCP servers, discovers their tools, and
//! dispatches tool calls through the MCP protocol. When a server
//! sends `notifications/tools/list_changed`, the client automatically
//! rediscovers tools and updates the registry.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use hermes_core::{JsonSchema, ToolSchema};
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

const DEFAULT_MCP_CALL_TIMEOUT_SECS: u64 = 60;
const MAX_MCP_CALL_TIMEOUT_SECS: u64 = 900;
const STALE_TRANSPORT_MARKERS: &[&str] = &[
    "closedresourceerror",
    "closed resource",
    "transport is closed",
    "connection closed",
    "broken pipe",
    "end of file",
    "eof",
];

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
    /// Optional sampling policy for server-initiated LLM requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingConfig>,
    /// Optional authentication provider for remote servers.
    #[serde(skip)]
    pub auth_provider: Option<Arc<dyn McpAuthProvider>>,
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
            .field("sampling", &self.sampling)
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
            && self.sampling == other.sampling
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
            sampling: None,
            auth_provider: None,
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
            sampling: None,
            auth_provider: None,
        }
    }

    /// Set explicit parallel-tool-call capability for this server.
    pub fn with_parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.supports_parallel_tool_calls = enabled;
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

    /// Returns true if this config is for a stdio (local process) connection.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    /// Returns true if this config is for an HTTP (remote) connection.
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }
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
    pub input_schema: JsonSchema,
}

/// Response from tools/list method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ToolsListResponse {
    pub tools: Vec<McpToolDefinition>,
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
}

impl McpClient {
    /// Create a new client for the given config. Does not connect yet.
    pub fn new(config: McpServerConfig) -> Self {
        let sampling_config = config.sampling.clone();
        Self {
            config,
            transport: None,
            tools: Vec::new(),
            resources: Vec::new(),
            next_id: 1,
            connected: false,
            sampling_config,
            sampling_callback: None,
            sampling_rate_timestamps: VecDeque::new(),
            sampling_tool_rounds: 0,
            sampling_metrics: SamplingMetrics::default(),
            connected_at: None,
        }
    }

    /// Connect to the MCP server: start transport, perform initialize
    /// handshake, and discover available tools.
    pub async fn connect(&mut self) -> Result<(), McpError> {
        if self.connected {
            return Err(McpError::ConnectionError("Already connected".to_string()));
        }

        let transport = self.create_transport().await?;
        self.finish_connect_with_transport(transport).await
    }

    async fn finish_connect_with_transport(
        &mut self,
        mut transport: Box<dyn McpTransport>,
    ) -> Result<(), McpError> {
        transport.start().await?;
        self.transport = Some(transport);

        let discovery = match self.initialize().await {
            Ok(_) => self.discover_tools().await,
            Err(err) => Err(err),
        };
        if let Err(err) = discovery {
            self.connected = false;
            self.connected_at = None;
            self.tools.clear();
            self.resources.clear();
            if let Some(mut transport) = self.transport.take() {
                if let Err(close_err) = transport.close().await {
                    warn!(
                        "MCP transport close after failed connect also failed: {}",
                        close_err
                    );
                }
            }
            return Err(err);
        }

        self.connected = true;
        self.connected_at = Some(Instant::now());

        Ok(())
    }

    /// Disconnect from the MCP server and release resources.
    pub async fn disconnect(&mut self) -> Result<(), McpError> {
        if let Some(mut transport) = self.transport.take() {
            transport.close().await?;
        }
        self.connected = false;
        self.connected_at = None;
        self.tools.clear();
        self.resources.clear();
        Ok(())
    }

    /// Returns `true` if the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Discover (or re-discover) the tools this server exposes.
    ///
    /// Sends a `tools/list` JSON-RPC request and parses the response into
    /// a `Vec<ToolSchema>`. The result is also cached internally.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolSchema>, McpError> {
        let result = self
            .send_request("tools/list", serde_json::json!({}))
            .await?;

        let tools_response: ToolsListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        let tools: Vec<ToolSchema> = tools_response
            .tools
            .into_iter()
            .map(|t| ToolSchema {
                name: t.name,
                description: t.description.unwrap_or_default(),
                parameters: t.input_schema,
            })
            .collect();

        self.tools = tools.clone();
        Ok(tools)
    }

    /// Call a tool on this server by name with the given arguments.
    ///
    /// Sends a `tools/call` JSON-RPC request and returns the result. Text
    /// content items are joined into a single string value; other content
    /// types are returned as raw JSON.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });

        let timeout = mcp_call_timeout_duration();
        let started = Instant::now();
        let result =
            match tokio::time::timeout(timeout, self.send_request("tools/call", params)).await {
                Ok(res) => res?,
                Err(_) => {
                    let elapsed = started.elapsed().as_secs_f64();
                    return Err(McpError::ConnectionError(format!(
                        "MCP call timed out after {:.1}s (configured timeout: {:.1}s)",
                        elapsed,
                        timeout.as_secs_f64()
                    )));
                }
            };

        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let message = extract_mcp_error_message(&result);
            return Err(Self::classify_protocol_error(-1, &message));
        }

        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let mut parts: Vec<String> = Vec::new();
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        if !text.trim().is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                    continue;
                }
                if let Some(media_tag) = cache_mcp_image_block(item) {
                    parts.push(media_tag);
                }
            }
            if !parts.is_empty() {
                return Ok(serde_json::json!(parts.join("\n")));
            }
        }

        Ok(result)
    }

    /// List resources available on this server.
    pub async fn list_resources(&mut self) -> Result<Vec<ResourceInfo>, McpError> {
        let result = self
            .send_request("resources/list", serde_json::json!({}))
            .await?;

        let resources_response: ResourcesListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        self.resources = resources_response.resources.clone();
        Ok(resources_response.resources)
    }

    /// Read a single resource by URI from this server.
    pub async fn read_resource(&mut self, uri: &str) -> Result<Value, McpError> {
        let params = serde_json::json!({ "uri": uri });
        self.send_request("resources/read", params).await
    }

    /// Return the cached tool list from the last `list_tools` / `connect` call.
    pub fn cached_tools(&self) -> &[ToolSchema] {
        &self.tools
    }

    /// Return the cached resource list from the last `list_resources` call.
    pub fn cached_resources(&self) -> &[ResourceInfo] {
        &self.resources
    }

    /// Return the uptime of this connection, if connected.
    pub fn uptime(&self) -> Option<std::time::Duration> {
        self.connected_at.map(|t| t.elapsed())
    }

    /// Set the sampling configuration for server-initiated LLM requests.
    pub fn set_sampling_config(&mut self, config: SamplingConfig) {
        self.sampling_config = Some(config);
    }

    /// Set the callback used to satisfy MCP `sampling/createMessage` requests.
    pub fn set_sampling_callback(&mut self, callback: LlmCallback) {
        self.sampling_callback = Some(callback);
    }

    /// Clear the sampling callback for this client.
    pub fn clear_sampling_callback(&mut self) {
        self.sampling_callback = None;
    }

    /// Return sampling audit counters for this client.
    pub fn sampling_metrics(&self) -> &SamplingMetrics {
        &self.sampling_metrics
    }

    // -----------------------------------------------------------------------
    // Prompt support
    // -----------------------------------------------------------------------

    /// List prompts available on this server.
    pub async fn list_prompts(&mut self) -> Result<Vec<PromptInfo>, McpError> {
        let result = self
            .send_request("prompts/list", serde_json::json!({}))
            .await?;

        let response: PromptsListResponse =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        Ok(response.prompts)
    }

    /// Get a prompt by name with the given arguments.
    pub async fn get_prompt(
        &mut self,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args,
        });

        let result = self.send_request("prompts/get", params).await?;
        let prompt_result: PromptResult =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        Ok(prompt_result)
    }

    // -----------------------------------------------------------------------
    // Sampling support (server-initiated LLM requests)
    // -----------------------------------------------------------------------

    /// Handle a sampling request from the MCP server.
    ///
    /// The server can ask the client to invoke an LLM on its behalf.
    /// The `llm_callback` performs the actual LLM call.
    pub async fn handle_sampling_request(
        &mut self,
        params: Value,
        llm_callback: &LlmCallback,
    ) -> Result<Value, McpError> {
        self.sampling_metrics.requests += 1;
        let config = self.sampling_config.clone().ok_or_else(|| {
            McpError::Config("Sampling not configured on this client".to_string())
        })?;
        if !config.enabled {
            self.sampling_metrics.errors += 1;
            return Err(McpError::Forbidden(
                "Sampling is disabled on this client".to_string(),
            ));
        }
        if !self.check_sampling_rate_limit(config.max_rpm) {
            self.sampling_metrics.errors += 1;
            self.sampling_metrics.rate_limited += 1;
            return Err(McpError::Forbidden(format!(
                "Sampling rate limit exceeded (max {} requests/minute)",
                config.max_rpm
            )));
        }

        let model = self.resolve_sampling_model(&params, &config);

        if !config.allowed_models.is_empty()
            && !config.allowed_models.iter().any(|m| m.as_str() == model)
        {
            self.sampling_metrics.errors += 1;
            return Err(McpError::InvalidParams(format!(
                "Model '{}' is not in the allowed list",
                model
            )));
        }

        let max_tokens = params
            .get("maxTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(config.max_tokens_cap as u64)
            .min(config.max_tokens_cap as u64);

        let messages = params
            .get("messages")
            .cloned()
            .unwrap_or(serde_json::json!([]));
        let openai_messages = Self::convert_mcp_messages_to_openai(&messages);

        let mut llm_request = serde_json::json!({
            "model": model,
            "messages": openai_messages,
            "max_tokens": max_tokens,
        });
        if let Some(system_prompt) = params.get("systemPrompt").and_then(Value::as_str) {
            if let Some(obj) = llm_request.as_object_mut() {
                obj.insert(
                    "system_prompt".to_string(),
                    Value::String(system_prompt.to_string()),
                );
            }
        }
        if let Some(temperature) = params.get("temperature").and_then(Value::as_f64) {
            if let Some(obj) = llm_request.as_object_mut() {
                obj.insert("temperature".to_string(), serde_json::json!(temperature));
            }
        }
        if let Some(stop_sequences) = params.get("stopSequences").cloned() {
            if let Some(obj) = llm_request.as_object_mut() {
                obj.insert("stop".to_string(), stop_sequences);
            }
        }

        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        let result = match tokio::time::timeout(timeout, llm_callback(llm_request)).await {
            Ok(Ok(value)) => value,
            Ok(Err(err)) => {
                self.sampling_metrics.errors += 1;
                return Err(err);
            }
            Err(_) => {
                self.sampling_metrics.errors += 1;
                return Err(McpError::ConnectionError(
                    "Sampling LLM callback timed out".into(),
                ));
            }
        };

        self.sampling_metrics.tokens_used += result
            .get("usage")
            .and_then(|u| u.get("total_tokens").or_else(|| u.get("totalTokens")))
            .and_then(Value::as_u64)
            .unwrap_or(0);

        match self.build_sampling_response(&result, &model, &config) {
            Ok(value) => Ok(value),
            Err(err) => {
                self.sampling_metrics.errors += 1;
                Err(err)
            }
        }
    }

    async fn handle_configured_sampling_request(
        &mut self,
        params: Value,
    ) -> Result<Value, McpError> {
        let callback = self.sampling_callback.clone().ok_or_else(|| {
            McpError::NotConfigured("Sampling callback is not configured".to_string())
        })?;
        self.handle_sampling_request(params, &callback).await
    }

    fn check_sampling_rate_limit(&mut self, max_rpm: u32) -> bool {
        if max_rpm == 0 {
            return false;
        }
        let now = Instant::now();
        while self
            .sampling_rate_timestamps
            .front()
            .is_some_and(|stamp| now.duration_since(*stamp) > Duration::from_secs(60))
        {
            self.sampling_rate_timestamps.pop_front();
        }
        if self.sampling_rate_timestamps.len() >= max_rpm as usize {
            return false;
        }
        self.sampling_rate_timestamps.push_back(now);
        true
    }

    fn resolve_sampling_model(&self, params: &Value, config: &SamplingConfig) -> String {
        if let Some(model) = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return model.to_string();
        }
        if let Some(model) = params.get("model").and_then(Value::as_str) {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        params
            .get("modelPreferences")
            .and_then(|prefs| prefs.get("hints"))
            .and_then(Value::as_array)
            .and_then(|hints| hints.first())
            .and_then(|hint| hint.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("default")
            .to_string()
    }

    fn build_sampling_response(
        &mut self,
        result: &Value,
        request_model: &str,
        config: &SamplingConfig,
    ) -> Result<Value, McpError> {
        let choice = result
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or_else(|| {
                McpError::Serialization("Sampling response missing choices[0]".into())
            })?;
        let message = choice
            .get("message")
            .ok_or_else(|| McpError::Serialization("Sampling response missing message".into()))?;
        let response_model = result
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(request_model);
        if let Some(tool_calls) = message
            .get("tool_calls")
            .or_else(|| message.get("toolCalls"))
            .and_then(Value::as_array)
            .filter(|calls| !calls.is_empty())
        {
            return self.build_sampling_tool_use_response(tool_calls, response_model, config);
        }

        self.sampling_tool_rounds = 0;
        let content = message.get("content").and_then(Value::as_str).unwrap_or("");
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant");
        let stop_reason = match choice
            .get("finish_reason")
            .or_else(|| choice.get("finishReason"))
            .and_then(Value::as_str)
            .unwrap_or("stop")
        {
            "length" | "max_tokens" | "maxTokens" => "maxTokens",
            "tool_calls" | "toolUse" => "toolUse",
            _ => "endTurn",
        };

        Ok(serde_json::json!({
            "role": role,
            "content": {
                "type": "text",
                "text": content,
            },
            "model": response_model,
            "stopReason": stop_reason,
        }))
    }

    fn build_sampling_tool_use_response(
        &mut self,
        tool_calls: &[Value],
        response_model: &str,
        config: &SamplingConfig,
    ) -> Result<Value, McpError> {
        self.sampling_metrics.tool_use_count += tool_calls.len() as u64;
        if config.max_tool_rounds == 0 {
            self.sampling_tool_rounds = 0;
            return Err(McpError::Forbidden(
                "Tool loops disabled for sampling (max_tool_rounds=0)".to_string(),
            ));
        }
        self.sampling_tool_rounds += 1;
        if self.sampling_tool_rounds > config.max_tool_rounds {
            self.sampling_tool_rounds = 0;
            return Err(McpError::Forbidden(format!(
                "Tool loop limit exceeded for sampling (max {} rounds)",
                config.max_tool_rounds
            )));
        }

        let content: Vec<Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| {
                let function = call.get("function").unwrap_or(call);
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown_tool");
                let raw_args = function
                    .get("arguments")
                    .or_else(|| function.get("input"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let input = match raw_args {
                    Value::String(raw) => serde_json::from_str::<Value>(&raw)
                        .unwrap_or_else(|_| serde_json::json!({ "_raw": raw })),
                    Value::Object(_) => raw_args,
                    other => serde_json::json!({ "_raw": other.to_string() }),
                };
                serde_json::json!({
                    "type": "tool_use",
                    "id": call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{idx}")),
                    "name": name,
                    "input": input,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "role": "assistant",
            "content": content,
            "model": response_model,
            "stopReason": "toolUse",
        }))
    }

    fn convert_mcp_messages_to_openai(messages: &Value) -> Value {
        let arr = match messages.as_array() {
            Some(a) => a,
            None => return serde_json::json!([]),
        };

        let mut converted: Vec<Value> = Vec::new();
        for msg in arr {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let Some(content) = msg.get("content") else {
                converted.push(serde_json::json!({"role": role, "content": ""}));
                continue;
            };
            if let Some(text) = content.as_str() {
                converted.push(serde_json::json!({"role": role, "content": text}));
                continue;
            }
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                converted.push(serde_json::json!({"role": role, "content": text}));
                continue;
            }
            let Some(blocks) = content.as_array() else {
                converted.push(serde_json::json!({"role": role, "content": ""}));
                continue;
            };

            let mut text_parts: Vec<String> = Vec::new();
            let mut image_parts: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for block in blocks {
                let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                if block.get("toolUseId").is_some() || block_type == "tool_result" {
                    let tool_call_id = block
                        .get("toolUseId")
                        .or_else(|| block.get("tool_use_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool_result");
                    let tool_text = Self::sampling_block_text(block);
                    converted.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": tool_text,
                    }));
                    continue;
                }
                if block_type == "tool_use"
                    || (block.get("name").is_some() && block.get("input").is_some())
                {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown_tool");
                    let input = block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    tool_calls.push(serde_json::json!({
                        "id": block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool_call"),
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string(),
                        },
                    }));
                    continue;
                }
                if block_type == "image" {
                    if let Some(data) = block.get("data").and_then(Value::as_str) {
                        let mime = block
                            .get("mimeType")
                            .or_else(|| block.get("mime_type"))
                            .and_then(Value::as_str)
                            .unwrap_or("image/png");
                        image_parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime};base64,{data}"),
                            },
                        }));
                    }
                    continue;
                }
                let text = Self::sampling_block_text(block);
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }

            if !tool_calls.is_empty() {
                let mut message = serde_json::json!({
                    "role": role,
                    "tool_calls": tool_calls,
                });
                if !text_parts.is_empty() {
                    message["content"] = Value::String(text_parts.join("\n"));
                }
                converted.push(message);
            } else if image_parts.is_empty() {
                converted.push(serde_json::json!({
                    "role": role,
                    "content": text_parts.join("\n"),
                }));
            } else {
                let mut parts = Vec::new();
                if !text_parts.is_empty() {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": text_parts.join("\n"),
                    }));
                }
                parts.extend(image_parts);
                converted.push(serde_json::json!({
                    "role": role,
                    "content": parts,
                }));
            }
        }

        Value::Array(converted)
    }

    fn sampling_block_text(block: &Value) -> String {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            return text.to_string();
        }
        if let Some(content) = block.get("content") {
            if let Some(text) = content.as_str() {
                return text.to_string();
            }
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(items) = content.as_array() {
                return items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        String::new()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Build the transport from the stored config.
    async fn create_transport(&self) -> Result<Box<dyn McpTransport>, McpError> {
        if self.config.is_stdio() {
            let command = self
                .config
                .command
                .as_ref()
                .ok_or_else(|| McpError::Config("stdio config missing command".to_string()))?;
            Ok(Box::new(StdioTransport::new(
                command,
                &self.config.args,
                &self.config.env,
            )))
        } else if self.config.is_http() {
            let url = self
                .config
                .url
                .as_ref()
                .ok_or_else(|| McpError::Config("http config missing url".to_string()))?;
            let auth_token = if let Some(ref provider) = self.config.auth_provider {
                Some(provider.get_token().await?)
            } else {
                None
            };
            Ok(Box::new(HttpSseTransport::new(url, auth_token)))
        } else {
            Err(McpError::Config(
                "server config must specify either command (stdio) or url (http)".to_string(),
            ))
        }
    }

    /// Send a JSON-RPC request and return the `result` field from the response.
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        self.transport_mut()?.send(request).await?;
        loop {
            let response = self.transport_mut()?.receive().await?;
            if Self::response_matches_id(&response, id) {
                return Self::parse_jsonrpc_result(response);
            }
            if response.get("method").and_then(Value::as_str).is_some() {
                if let Some(reply) = self.handle_server_request_message(response).await {
                    self.transport_mut()?.send(reply).await?;
                }
                continue;
            }
            debug!(
                "Ignoring MCP message while waiting for response id {}: {}",
                id, response
            );
        }
    }

    fn transport_mut(&mut self) -> Result<&mut Box<dyn McpTransport>, McpError> {
        self.transport
            .as_mut()
            .ok_or_else(|| McpError::ConnectionError("Not connected".to_string()))
    }

    fn response_matches_id(response: &Value, expected_id: u64) -> bool {
        response
            .get("id")
            .and_then(Value::as_u64)
            .is_some_and(|id| id == expected_id)
            || response
                .get("id")
                .and_then(Value::as_i64)
                .is_some_and(|id| id >= 0 && id as u64 == expected_id)
    }

    fn parse_jsonrpc_result(response: Value) -> Result<Value, McpError> {
        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let raw_message = error.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let message = if raw_message.trim().is_empty() {
                format!("ProtocolError(code={code})")
            } else {
                raw_message.to_string()
            };
            return Err(Self::classify_protocol_error(code, message));
        }

        response.get("result").cloned().ok_or(McpError::Protocol {
            code: -1,
            message: "Missing result in response".to_string(),
        })
    }

    async fn handle_server_request_message(&mut self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = id else {
            debug!("Ignoring MCP notification from server: {}", method);
            return None;
        };
        let result = match method {
            "sampling/createMessage" => {
                let params = message
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                self.handle_configured_sampling_request(params).await
            }
            _ => Err(McpError::MethodNotFound(format!(
                "Unsupported server-initiated MCP method: {method}"
            ))),
        };
        Some(match result {
            Ok(result) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }),
            Err(err) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": Self::jsonrpc_code_for_error(&err),
                    "message": err.to_string(),
                },
            }),
        })
    }

    fn jsonrpc_code_for_error(err: &McpError) -> i64 {
        match err {
            McpError::MethodNotFound(_) => -32601,
            McpError::InvalidParams(_) => -32602,
            McpError::NotConfigured(_) => -32001,
            McpError::Forbidden(_) => -32600,
            McpError::Serialization(_) => -32700,
            _ => -32000,
        }
    }

    fn classify_protocol_error(code: i64, message: impl AsRef<str>) -> McpError {
        let message = message.as_ref().trim();
        let normalized_message = if message.is_empty() {
            format!("ProtocolError(code={code})")
        } else {
            message.to_string()
        };
        let msg_lc = normalized_message.to_ascii_lowercase();
        if code == -32601 {
            return McpError::MethodNotFound(normalized_message);
        }
        if code == -32602 {
            return McpError::InvalidParams(normalized_message);
        }
        if code == -32600 || msg_lc.contains("forbidden") || msg_lc.contains("permission denied") {
            return McpError::Forbidden(normalized_message);
        }
        if code == -32001 {
            return McpError::NotConfigured(normalized_message);
        }
        if msg_lc.contains("not configured")
            || msg_lc.contains("missing config")
            || msg_lc.contains("missing command")
            || msg_lc.contains("missing url")
        {
            return McpError::NotConfigured(normalized_message);
        }
        if msg_lc.contains("not found") || msg_lc.contains("unknown method") {
            return McpError::ResourceNotFound(normalized_message);
        }
        McpError::Protocol {
            code,
            message: normalized_message,
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| McpError::ConnectionError("Not connected".to_string()))?;

        transport.send(notification).await?;
        Ok(())
    }

    /// Run the MCP initialize handshake.
    async fn initialize(&mut self) -> Result<InitializeResult, McpError> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": {},
            },
            "clientInfo": {
                "name": "hermes-agent",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let result = self.send_request("initialize", params).await?;
        let init_result: InitializeResult =
            serde_json::from_value(result).map_err(|e| McpError::Serialization(e.to_string()))?;

        self.send_notification("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(init_result)
    }

    /// Internal alias used during connect().
    async fn discover_tools(&mut self) -> Result<(), McpError> {
        self.list_tools().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// McpManager — manages multiple McpClient instances
// ---------------------------------------------------------------------------

/// Manages connections to multiple MCP servers.
///
/// The manager can:
/// - Connect to local (stdio) or remote (HTTP/SSE) MCP servers
/// - Discover available tools on each server
/// - Call tools on connected servers
/// - List and read resources from servers
/// - Automatically update the tool registry when servers notify of changes
pub struct McpManager {
    /// Active client connections keyed by server name.
    clients: HashMap<String, McpClient>,
    /// Shared tool registry for discovered tools.
    tool_registry: Arc<ToolRegistry>,
    sampling_config: Option<SamplingConfig>,
    sampling_callback: Option<LlmCallback>,
}

impl McpManager {
    /// Create a new manager with the given tool registry.
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            clients: HashMap::new(),
            tool_registry,
            sampling_config: None,
            sampling_callback: None,
        }
    }

    /// Connect to an MCP server.
    ///
    /// Creates an `McpClient`, connects it, and registers the discovered
    /// tools in the shared tool registry with names prefixed by the server
    /// name (e.g. `"server_name__tool_name"`).
    pub async fn connect(&mut self, name: &str, config: McpServerConfig) -> Result<(), McpError> {
        info!("Connecting to MCP server: {}", name);

        let mut client = McpClient::new(config);
        if let Some(config) = self.sampling_config.clone() {
            client.set_sampling_config(config);
        }
        if let Some(callback) = self.sampling_callback.clone() {
            client.set_sampling_callback(callback);
        }
        client.connect().await?;

        let tools = client.cached_tools();
        debug!("Discovered {} tools from server '{}'", tools.len(), name);

        for tool in tools {
            let prefixed_name = format!("{}__{}", name, tool.name);
            debug!("Registered MCP tool: {}", prefixed_name);
        }

        self.clients.insert(name.to_string(), client);
        Ok(())
    }

    /// Connect and discover multiple MCP servers concurrently.
    ///
    /// Each server gets its own connection future, so one slow MCP server no
    /// longer consumes the discovery budget for every other configured server.
    /// The returned reports are intended for user-visible startup summaries.
    pub async fn connect_all_parallel(
        &mut self,
        configs: Vec<(String, McpServerConfig)>,
    ) -> Vec<McpDiscoveryReport> {
        let mut reports: Vec<(usize, McpDiscoveryReport)> = Vec::new();
        let mut tasks = tokio::task::JoinSet::new();

        for (index, (name, config)) in configs.into_iter().enumerate() {
            if let Some(existing) = self.clients.get(&name) {
                reports.push((
                    index,
                    McpDiscoveryReport {
                        name,
                        connected: existing.is_connected(),
                        transport_type: transport_type_for_config(&existing.config).to_string(),
                        tool_count: existing.cached_tools().len(),
                        error: None,
                    },
                ));
                continue;
            }

            let sampling_config = self.sampling_config.clone();
            let sampling_callback = self.sampling_callback.clone();
            tasks.spawn(async move {
                let transport_type = transport_type_for_config(&config).to_string();
                let mut client = McpClient::new(config);
                if let Some(config) = sampling_config {
                    client.set_sampling_config(config);
                }
                if let Some(callback) = sampling_callback {
                    client.set_sampling_callback(callback);
                }
                let result = client.connect().await;
                match result {
                    Ok(()) => {
                        let tool_count = client.cached_tools().len();
                        (
                            index,
                            McpDiscoveryReport {
                                name,
                                connected: true,
                                transport_type,
                                tool_count,
                                error: None,
                            },
                            Some(client),
                        )
                    }
                    Err(err) => {
                        warn!("Failed to connect to MCP server '{}': {}", name, err);
                        (
                            index,
                            McpDiscoveryReport {
                                name,
                                connected: false,
                                transport_type,
                                tool_count: 0,
                                error: Some(err.to_string()),
                            },
                            None,
                        )
                    }
                }
            });
        }

        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((index, report, client)) => {
                    if let Some(client) = client {
                        debug!(
                            "Discovered {} tools from MCP server '{}'",
                            report.tool_count, report.name
                        );
                        self.clients.insert(report.name.clone(), client);
                    }
                    reports.push((index, report));
                }
                Err(err) => {
                    reports.push((
                        usize::MAX,
                        McpDiscoveryReport {
                            name: "mcp-discovery-task".to_string(),
                            connected: false,
                            transport_type: "unknown".to_string(),
                            tool_count: 0,
                            error: Some(format!("discovery task failed: {err}")),
                        },
                    ));
                }
            }
        }

        reports.sort_by_key(|(index, _)| *index);
        let summary = reports
            .iter()
            .fold((0usize, 0usize), |(tools, failed), (_, report)| {
                (
                    tools + report.tool_count,
                    failed + usize::from(!report.connected),
                )
            });
        info!(
            "MCP discovery complete: {} tool(s), {} failed server(s)",
            summary.0, summary.1
        );
        reports.into_iter().map(|(_, report)| report).collect()
    }

    /// Disconnect from an MCP server and remove it from the active list.
    pub async fn disconnect(&mut self, name: &str) -> Result<(), McpError> {
        if let Some(mut client) = self.clients.remove(name) {
            info!("Disconnecting from MCP server: {}", name);
            client.disconnect().await?;
            Ok(())
        } else {
            Err(McpError::ServerNotFound(name.to_string()))
        }
    }

    /// Disconnect all servers.
    pub async fn disconnect_all(&mut self) -> Result<(), McpError> {
        let names: Vec<String> = self.clients.keys().cloned().collect();
        for name in names {
            self.disconnect(&name).await?;
        }
        Ok(())
    }

    /// Check if a server is connected.
    pub fn is_connected(&self, name: &str) -> bool {
        self.clients.get(name).map_or(false, |c| c.is_connected())
    }

    /// Get the list of connected server names.
    pub fn connected_servers(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// Discover (or re-discover) tools on a connected server.
    pub async fn discover_tools(&mut self, server_name: &str) -> Result<Vec<ToolSchema>, McpError> {
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_tools().await
    }

    /// Call a tool on a connected MCP server.
    pub async fn call_tool(
        &mut self,
        server_name: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<Value, McpError> {
        let reconnect_config: McpServerConfig = {
            let client = self
                .clients
                .get_mut(server_name)
                .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
            match client.call_tool(tool_name, args.clone()).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    if is_stale_transport_error(&err) {
                        warn!(
                            "MCP stale transport detected on '{}' ({}); reconnecting once",
                            server_name, err
                        );
                        client.config.clone()
                    } else {
                        return Err(err);
                    }
                }
            }
        };
        let config = reconnect_config;
        let _ = self.disconnect(server_name).await;
        self.connect(server_name, config).await?;
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.call_tool(tool_name, args).await
    }

    /// List resources available on a connected server.
    pub async fn list_resources(
        &mut self,
        server_name: &str,
    ) -> Result<Vec<ResourceInfo>, McpError> {
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_resources().await
    }

    /// Read a resource from a connected server.
    pub async fn read_resource(&mut self, server_name: &str, uri: &str) -> Result<Value, McpError> {
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.read_resource(uri).await
    }

    /// Handle a `tools/list_changed` notification from a server.
    ///
    /// Re-discovers tools from the server and updates the registry.
    pub async fn handle_tools_changed(
        &mut self,
        server_name: &str,
    ) -> Result<Vec<ToolSchema>, McpError> {
        info!(
            "Handling tools/list_changed notification from '{}'",
            server_name
        );
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let tools = client.list_tools().await?;
        debug!(
            "Re-discovered {} tools from server '{}'",
            tools.len(),
            server_name
        );
        Ok(tools)
    }

    /// Get a mutable reference to a specific client.
    pub fn get_client_mut(&mut self, name: &str) -> Option<&mut McpClient> {
        self.clients.get_mut(name)
    }

    /// Get a reference to the tool registry.
    pub fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    // -----------------------------------------------------------------------
    // Sampling
    // -----------------------------------------------------------------------

    /// Set the sampling configuration for all connected clients.
    pub fn set_sampling_config(&mut self, config: SamplingConfig) {
        self.sampling_config = Some(config.clone());
        for client in self.clients.values_mut() {
            client.set_sampling_config(config.clone());
        }
    }

    /// Set the sampling callback for all connected and future clients.
    pub fn set_sampling_callback(&mut self, callback: LlmCallback) {
        self.sampling_callback = Some(callback.clone());
        for client in self.clients.values_mut() {
            client.set_sampling_callback(callback.clone());
        }
    }

    /// Return sampling audit counters for a connected server.
    pub fn sampling_metrics(&self, server_name: &str) -> Option<&SamplingMetrics> {
        self.clients
            .get(server_name)
            .map(McpClient::sampling_metrics)
    }

    // -----------------------------------------------------------------------
    // Prompts
    // -----------------------------------------------------------------------

    /// List prompts available on a connected server.
    pub async fn list_prompts(&mut self, server_name: &str) -> Result<Vec<PromptInfo>, McpError> {
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.list_prompts().await
    }

    /// Get a prompt from a connected server.
    pub async fn get_prompt(
        &mut self,
        server_name: &str,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        client.get_prompt(name, args).await
    }

    // -----------------------------------------------------------------------
    // Status / probe
    // -----------------------------------------------------------------------

    /// Get the status of all connected MCP servers.
    pub fn get_status(&self) -> HashMap<String, McpServerStatus> {
        self.clients
            .iter()
            .map(|(name, client)| {
                let transport_type = if client.config.is_stdio() {
                    "stdio".to_string()
                } else if client.config.is_http() {
                    "http".to_string()
                } else {
                    "unknown".to_string()
                };
                let status = McpServerStatus {
                    name: name.clone(),
                    connected: client.is_connected(),
                    tool_count: client.cached_tools().len(),
                    resource_count: client.cached_resources().len(),
                    transport_type,
                    uptime_secs: client.uptime().map(|d| d.as_secs()),
                };
                (name.clone(), status)
            })
            .collect()
    }

    /// Probe a connected MCP server to check reachability and discover capabilities.
    pub async fn probe_server(&mut self, name: &str) -> Result<McpProbeResult, McpError> {
        let client = self
            .clients
            .get_mut(name)
            .ok_or_else(|| McpError::ServerNotFound(name.to_string()))?;

        let start = Instant::now();
        let tools_result = client.list_tools().await;
        let latency = start.elapsed();

        match tools_result {
            Ok(tools) => {
                let resources = client.list_resources().await.unwrap_or_default();

                Ok(McpProbeResult {
                    reachable: true,
                    latency_ms: latency.as_millis() as u64,
                    tools: tools.iter().map(|t| t.name.clone()).collect(),
                    resources: resources.iter().map(|r| r.uri.clone()).collect(),
                    server_info: None,
                })
            }
            Err(e) => {
                warn!("Probe failed for server '{}': {}", name, e);
                Ok(McpProbeResult {
                    reachable: false,
                    latency_ms: latency.as_millis() as u64,
                    tools: vec![],
                    resources: vec![],
                    server_info: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cache_mcp_image_block, is_stale_transport_error, LlmCallback, McpClient, McpManager,
        McpServerConfig, SamplingConfig,
    };
    use crate::transport::McpTransport;
    use crate::McpError;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct FakeTransport {
        responses: VecDeque<serde_json::Value>,
        closed: Arc<AtomicBool>,
        sent: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<serde_json::Value>, closed: Arc<AtomicBool>) -> Self {
            Self::new_with_sent(responses, closed, Arc::new(Mutex::new(Vec::new())))
        }

        fn new_with_sent(
            responses: Vec<serde_json::Value>,
            closed: Arc<AtomicBool>,
            sent: Arc<Mutex<Vec<serde_json::Value>>>,
        ) -> Self {
            Self {
                responses: responses.into(),
                closed,
                sent,
            }
        }
    }

    #[async_trait]
    impl McpTransport for FakeTransport {
        async fn start(&mut self) -> Result<(), McpError> {
            Ok(())
        }

        async fn send(&mut self, message: serde_json::Value) -> Result<(), McpError> {
            self.sent.lock().expect("sent lock").push(message);
            Ok(())
        }

        async fn receive(&mut self) -> Result<serde_json::Value, McpError> {
            self.responses
                .pop_front()
                .ok_or_else(|| McpError::ConnectionError("no fake response".to_string()))
        }

        async fn close(&mut self) -> Result<(), McpError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn classify_protocol_error_maps_forbidden() {
        let err = McpClient::classify_protocol_error(-32600, "Forbidden: capability missing");
        assert!(matches!(err, McpError::Forbidden(_)));
    }

    #[test]
    fn classify_protocol_error_maps_not_configured() {
        let err = McpClient::classify_protocol_error(-32001, "Not configured: prompts disabled");
        assert!(matches!(err, McpError::NotConfigured(_)));
    }

    #[test]
    fn classify_protocol_error_maps_not_found() {
        let err = McpClient::classify_protocol_error(-1, "resource not found");
        assert!(matches!(err, McpError::ResourceNotFound(_)));
    }

    #[test]
    fn classify_protocol_error_falls_back_when_message_empty() {
        let err = McpClient::classify_protocol_error(-32000, "");
        match err {
            McpError::Protocol { message, .. } => {
                assert!(message.contains("ProtocolError(code=-32000)"));
            }
            _ => panic!("expected protocol error"),
        }
    }

    #[test]
    fn stale_transport_marker_detection_matches_known_variants() {
        let err = McpError::ConnectionError("ClosedResourceError: ".to_string());
        assert!(is_stale_transport_error(&err));
        let err = McpError::ConnectionError("broken pipe while writing".to_string());
        assert!(is_stale_transport_error(&err));
        let err = McpError::ConnectionError("rate limited".to_string());
        assert!(!is_stale_transport_error(&err));
    }

    #[tokio::test]
    async fn connect_closes_transport_when_discovery_fails() {
        let closed = Arc::new(AtomicBool::new(false));
        let transport = FakeTransport::new(
            vec![
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": {"name": "fake", "version": "0"}
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "error": {"code": -32601, "message": "tools/list unavailable"}
                }),
            ],
            closed.clone(),
        );
        let mut client = McpClient::new(McpServerConfig::stdio("fake", Vec::new()));

        let err = client
            .finish_connect_with_transport(Box::new(transport))
            .await
            .expect_err("discovery should fail");

        assert!(matches!(err, McpError::MethodNotFound(_)));
        assert!(closed.load(Ordering::SeqCst));
        assert!(!client.is_connected());
        assert!(client.cached_tools().is_empty());
        assert!(client.cached_resources().is_empty());
    }

    #[tokio::test]
    async fn connect_all_parallel_reports_failed_servers_without_aborting_batch() {
        let registry = Arc::new(hermes_tools::ToolRegistry::new());
        let mut manager = McpManager::new(registry);

        let reports = manager
            .connect_all_parallel(vec![
                (
                    "missing-a".to_string(),
                    McpServerConfig::stdio("__hermes_missing_mcp_a__", Vec::new()),
                ),
                (
                    "missing-b".to_string(),
                    McpServerConfig::stdio("__hermes_missing_mcp_b__", Vec::new()),
                ),
            ])
            .await;

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "missing-a");
        assert_eq!(reports[1].name, "missing-b");
        assert!(reports.iter().all(|report| !report.connected));
        assert!(reports.iter().all(|report| report.tool_count == 0));
        assert!(reports.iter().all(|report| report.error.is_some()));
        assert!(!manager.is_connected("missing-a"));
        assert!(!manager.is_connected("missing-b"));
    }

    #[tokio::test]
    async fn sampling_request_applies_model_cap_rate_limit_and_metrics() {
        let captured = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let captured_for_callback = captured.clone();
        let callback: LlmCallback = Arc::new(move |request| {
            captured_for_callback
                .lock()
                .expect("captured lock")
                .push(request);
            Box::pin(async move {
                Ok(json!({
                    "model": "sample-model",
                    "choices": [{
                        "finish_reason": "length",
                        "message": {
                            "role": "assistant",
                            "content": "sampled text"
                        }
                    }],
                    "usage": {"total_tokens": 17}
                }))
            })
        });
        let mut client = McpClient::new(McpServerConfig::stdio("fake", Vec::new()));
        client.set_sampling_config(SamplingConfig {
            max_rpm: 1,
            max_tokens_cap: 64,
            allowed_models: vec!["sample-model".to_string()],
            ..SamplingConfig::default()
        });

        let result = client
            .handle_sampling_request(
                json!({
                    "model": "sample-model",
                    "maxTokens": 4096,
                    "messages": [{"role": "user", "content": {"text": "hello"}}]
                }),
                &callback,
            )
            .await
            .expect("sampling response");

        assert_eq!(result["content"]["text"], "sampled text");
        assert_eq!(result["stopReason"], "maxTokens");
        assert_eq!(client.sampling_metrics().requests, 1);
        assert_eq!(client.sampling_metrics().tokens_used, 17);
        let request = captured.lock().expect("captured lock")[0].clone();
        assert_eq!(request["max_tokens"], 64);
        assert_eq!(request["messages"][0]["content"], "hello");

        let err = client
            .handle_sampling_request(
                json!({
                    "model": "sample-model",
                    "messages": [{"role": "user", "content": "again"}]
                }),
                &callback,
            )
            .await
            .expect_err("second request should hit max_rpm=1");
        assert!(matches!(err, McpError::Forbidden(_)));
        assert_eq!(client.sampling_metrics().rate_limited, 1);
    }

    #[tokio::test]
    async fn sampling_config_can_be_carried_by_server_config() {
        let callback: LlmCallback = Arc::new(|_request| {
            Box::pin(async move {
                Ok(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "configured"
                        }
                    }]
                }))
            })
        });
        let mut client = McpClient::new(
            McpServerConfig::stdio("fake", Vec::new()).with_sampling_config(SamplingConfig {
                model: Some("configured-model".to_string()),
                allowed_models: vec!["configured-model".to_string()],
                ..SamplingConfig::default()
            }),
        );

        let result = client
            .handle_sampling_request(json!({"messages": []}), &callback)
            .await
            .expect("server config sampling policy should be active");

        assert_eq!(result["model"], "configured-model");
        assert_eq!(result["content"]["text"], "configured");
    }

    #[tokio::test]
    async fn sampling_tool_use_enforces_tool_round_limit() {
        let callback: LlmCallback = Arc::new(|_request| {
            Box::pin(async move {
                Ok(json!({
                    "model": "tool-model",
                    "choices": [{
                        "finish_reason": "tool_calls",
                        "message": {
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "call_weather",
                                "type": "function",
                                "function": {
                                    "name": "weather",
                                    "arguments": "{\"city\":\"Denver\"}"
                                }
                            }]
                        }
                    }],
                    "usage": {"total_tokens": 5}
                }))
            })
        });
        let mut client = McpClient::new(McpServerConfig::stdio("fake", Vec::new()));
        client.set_sampling_config(SamplingConfig {
            max_tool_rounds: 1,
            ..SamplingConfig::default()
        });

        let first = client
            .handle_sampling_request(json!({"messages": []}), &callback)
            .await
            .expect("first tool round allowed");
        assert_eq!(first["stopReason"], "toolUse");
        assert_eq!(first["content"][0]["name"], "weather");
        assert_eq!(first["content"][0]["input"]["city"], "Denver");

        let err = client
            .handle_sampling_request(json!({"messages": []}), &callback)
            .await
            .expect_err("second consecutive tool round should fail");
        assert!(matches!(err, McpError::Forbidden(_)));
        assert_eq!(client.sampling_metrics().tool_use_count, 2);
    }

    #[tokio::test]
    async fn send_request_replies_to_sampling_request_then_continues_waiting() {
        let callback: LlmCallback = Arc::new(|request| {
            Box::pin(async move {
                assert_eq!(request["messages"][0]["content"], "sample please");
                Ok(json!({
                    "model": "loop-model",
                    "choices": [{
                        "finish_reason": "stop",
                        "message": {
                            "role": "assistant",
                            "content": "sampled in loop"
                        }
                    }],
                    "usage": {"total_tokens": 11}
                }))
            })
        });
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let transport = FakeTransport::new_with_sent(
            vec![
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"sampling": {}},
                        "serverInfo": {"name": "fake", "version": "0"}
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": "sample-1",
                    "method": "sampling/createMessage",
                    "params": {
                        "model": "loop-model",
                        "messages": [{"role": "user", "content": {"text": "sample please"}}]
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {"tools": []}
                }),
            ],
            closed,
            sent.clone(),
        );
        let mut client = McpClient::new(McpServerConfig::stdio("fake", Vec::new()));
        client.set_sampling_config(SamplingConfig::default());
        client.set_sampling_callback(callback);

        client
            .finish_connect_with_transport(Box::new(transport))
            .await
            .expect("connect should handle sampling interleave");

        let sent_messages = sent.lock().expect("sent lock");
        let sampling_reply = sent_messages
            .iter()
            .find(|message| message.get("id") == Some(&json!("sample-1")))
            .expect("sampling reply should be sent");
        assert_eq!(
            sampling_reply["result"]["content"]["text"],
            "sampled in loop"
        );
        assert_eq!(client.sampling_metrics().requests, 1);
        assert_eq!(client.sampling_metrics().tokens_used, 11);
        assert!(client.is_connected());
    }

    #[test]
    fn cache_mcp_image_block_writes_media_file() {
        let td = TempDir::new().expect("tempdir");
        let old_home = std::env::var("HERMES_HOME").ok();
        std::env::set_var("HERMES_HOME", td.path().display().to_string());
        // 1x1 PNG.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO5Xn8cAAAAASUVORK5CYII=";
        let item = json!({
            "type": "image",
            "mimeType": "image/png",
            "data": png_b64
        });
        let media = cache_mcp_image_block(&item).expect("expected media tag");
        assert!(media.starts_with("MEDIA:"));
        let path = media.trim_start_matches("MEDIA:");
        assert!(
            std::path::Path::new(path).exists(),
            "cached media path should exist"
        );
        if let Some(prev) = old_home {
            std::env::set_var("HERMES_HOME", prev);
        } else {
            std::env::remove_var("HERMES_HOME");
        }
    }
}
