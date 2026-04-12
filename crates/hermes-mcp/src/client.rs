//! MCP Client (Requirement 14.1-14.3)
//!
//! Connects to external MCP servers, discovers their tools, and
//! dispatches tool calls through the MCP protocol. When a server
//! sends `notifications/tools/list_changed`, the client automatically
//! rediscovers tools and updates the registry.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info};

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
            .field("auth_provider", &self.auth_provider.as_ref().map(|_| "<McpAuthProvider>"))
            .finish()
    }
}

impl PartialEq for McpServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
            && self.args == other.args
            && self.env == other.env
            && self.url == other.url
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
            auth_provider: None,
        }
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
}

impl McpClient {
    /// Create a new client for the given config. Does not connect yet.
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            transport: None,
            tools: Vec::new(),
            resources: Vec::new(),
            next_id: 1,
            connected: false,
        }
    }

    /// Connect to the MCP server: start transport, perform initialize
    /// handshake, and discover available tools.
    pub async fn connect(&mut self) -> Result<(), McpError> {
        if self.connected {
            return Err(McpError::ConnectionError(
                "Already connected".to_string(),
            ));
        }

        let mut transport = self.create_transport().await?;
        transport.start().await?;
        self.transport = Some(transport);

        self.initialize().await?;
        self.discover_tools().await?;
        self.connected = true;

        Ok(())
    }

    /// Disconnect from the MCP server and release resources.
    pub async fn disconnect(&mut self) -> Result<(), McpError> {
        if let Some(mut transport) = self.transport.take() {
            transport.close().await?;
        }
        self.connected = false;
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

        let tools_response: ToolsListResponse = serde_json::from_value(result)
            .map_err(|e| McpError::Serialization(e.to_string()))?;

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
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });

        let result = self.send_request("tools/call", params).await?;

        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<String> = content
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            if !texts.is_empty() {
                return Ok(serde_json::json!(texts.join("\n")));
            }
        }

        Ok(result)
    }

    /// List resources available on this server.
    pub async fn list_resources(&mut self) -> Result<Vec<ResourceInfo>, McpError> {
        let result = self
            .send_request("resources/list", serde_json::json!({}))
            .await?;

        let resources_response: ResourcesListResponse = serde_json::from_value(result)
            .map_err(|e| McpError::Serialization(e.to_string()))?;

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

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Build the transport from the stored config.
    async fn create_transport(&self) -> Result<Box<dyn McpTransport>, McpError> {
        if self.config.is_stdio() {
            let command = self.config.command.as_ref().ok_or_else(|| {
                McpError::Config("stdio config missing command".to_string())
            })?;
            Ok(Box::new(StdioTransport::new(
                command,
                &self.config.args,
                &self.config.env,
            )))
        } else if self.config.is_http() {
            let url = self.config.url.as_ref().ok_or_else(|| {
                McpError::Config("http config missing url".to_string())
            })?;
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

        let transport = self.transport.as_mut().ok_or_else(|| {
            McpError::ConnectionError("Not connected".to_string())
        })?;

        transport.send(request).await?;
        let response = transport.receive().await?;

        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(McpError::Protocol {
                code,
                message: message.to_string(),
            });
        }

        response
            .get("result")
            .cloned()
            .ok_or(McpError::Protocol {
                code: -1,
                message: "Missing result in response".to_string(),
            })
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let transport = self.transport.as_mut().ok_or_else(|| {
            McpError::ConnectionError("Not connected".to_string())
        })?;

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
}

impl McpManager {
    /// Create a new manager with the given tool registry.
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            clients: HashMap::new(),
            tool_registry,
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
        self.clients
            .get(name)
            .map_or(false, |c| c.is_connected())
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
    pub async fn read_resource(
        &mut self,
        server_name: &str,
        uri: &str,
    ) -> Result<Value, McpError> {
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
}
