//! MCP Server (Requirement 14.4)
//!
//! Exposes hermes-agent tools as MCP tools to external clients.
//! The server handles the MCP protocol including:
//! - tools/list, tools/call
//! - resources/list, resources/read
//! - prompts/list

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use hermes_core::ToolSchema;
use hermes_tools::ToolRegistry;

use crate::client::ResourceInfo;
use crate::transport::McpTransport;
use crate::McpError;

// ---------------------------------------------------------------------------
// MCP tool format (for exposing to clients)
// ---------------------------------------------------------------------------

/// An MCP-format tool definition as specified by the Model Context Protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// An MCP prompt definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPromptInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

// ---------------------------------------------------------------------------
// McpServer
// ---------------------------------------------------------------------------

/// MCP server that exposes hermes-agent tools as MCP tools to external clients.
///
/// The server:
/// - Starts listening on the given transport
/// - Handles incoming JSON-RPC requests according to the MCP protocol
/// - Converts hermes-agent ToolSchema to MCP tool format
/// - Dispatches tool calls through the shared ToolRegistry
/// - Exposes resources and prompts
/// Capability flags controlling which MCP bridge operations are allowed.
#[derive(Debug, Clone)]
pub struct McpCapabilityPolicy {
    pub allow_tool_invoke: bool,
    pub allow_prompt_read: bool,
    pub allow_resource_read: bool,
}

impl Default for McpCapabilityPolicy {
    fn default() -> Self {
        Self {
            allow_tool_invoke: true,
            allow_prompt_read: true,
            allow_resource_read: true,
        }
    }
}

pub struct McpServer {
    /// Shared tool registry containing all hermes-agent tools.
    tool_registry: Arc<ToolRegistry>,
    /// Resources exposed by this server.
    resources: Vec<ResourceInfo>,
    /// Optional pre-registered resource contents by URI.
    resource_contents: HashMap<String, Vec<Value>>,
    /// Prompts exposed by this server.
    prompts: Vec<McpPromptInfo>,
    /// Server info.
    server_info: ServerInfo,
    /// Capability gating policy.
    capability_policy: McpCapabilityPolicy,
}

/// Server identity information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ServerInfo {
    name: String,
    version: String,
}

impl McpServer {
    /// Create a new MCP server with the given tool registry.
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            tool_registry,
            resources: Vec::new(),
            resource_contents: HashMap::new(),
            prompts: Vec::new(),
            server_info: ServerInfo {
                name: "hermes-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capability_policy: McpCapabilityPolicy::default(),
        }
    }

    /// Set the capability gating policy.
    pub fn with_capability_policy(mut self, policy: McpCapabilityPolicy) -> Self {
        self.capability_policy = policy;
        self
    }

    /// Add a resource to be exposed by this server.
    pub fn add_resource(&mut self, resource: ResourceInfo) {
        self.resources.push(resource);
    }

    /// Add a text resource and its content in one call.
    pub fn add_resource_text(&mut self, mut resource: ResourceInfo, text: impl Into<String>) {
        if resource.mime_type.is_none() {
            resource.mime_type = Some("text/plain".to_string());
        }
        let uri = resource.uri.clone();
        self.resources.push(resource.clone());
        self.resource_contents.insert(
            uri.clone(),
            vec![serde_json::json!({
                "uri": uri,
                "mimeType": resource.mime_type.clone().unwrap_or_else(|| "text/plain".to_string()),
                "text": text.into(),
            })],
        );
    }

    /// Replace resource contents for a URI.
    pub fn set_resource_contents(&mut self, uri: &str, contents: Vec<Value>) {
        self.resource_contents.insert(uri.to_string(), contents);
    }

    /// Add a prompt to be exposed by this server.
    pub fn add_prompt(&mut self, prompt: McpPromptInfo) {
        self.prompts.push(prompt);
    }

    /// Convert a hermes-agent ToolSchema to MCP tool format.
    fn tool_schema_to_mcp(schema: &ToolSchema) -> McpToolInfo {
        // Convert hermes-core JsonSchema to a JSON Value for MCP
        let input_schema = serde_json::to_value(&schema.parameters)
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        McpToolInfo {
            name: schema.name.clone(),
            description: Some(schema.description.clone()),
            input_schema,
        }
    }

    /// Handle an incoming JSON-RPC request.
    ///
    /// This is the main request dispatch method. It routes requests to
    /// the appropriate handler based on the method name.
    pub async fn handle_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        debug!("Handling MCP request: {}", method);

        match method {
            "initialize" => self.handle_initialize(params).await,
            "tools/list" => self.handle_tools_list(params).await,
            "tools/call" => self.handle_tools_call(params).await,
            "resources/list" => self.handle_resources_list(params).await,
            "resources/read" => self.handle_resources_read(params).await,
            "prompts/list" => self.handle_prompts_list(params).await,
            "prompts/get" => self.handle_prompts_get(params).await,
            "ping" => Ok(serde_json::json!({})),
            _ => {
                warn!("Unknown MCP method: {}", method);
                Err(McpError::MethodNotFound(method.to_string()))
            }
        }
    }

    /// Handle the initialize request.
    async fn handle_initialize(&self, _params: Value) -> Result<Value, McpError> {
        Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": {},
                "prompts": {},
            },
            "serverInfo": {
                "name": self.server_info.name,
                "version": self.server_info.version,
            }
        }))
    }

    /// Handle tools/list request.
    ///
    /// Returns all available tools from the registry in MCP format.
    async fn handle_tools_list(&self, _params: Value) -> Result<Value, McpError> {
        let definitions = self.tool_registry.get_definitions();
        let mcp_tools: Vec<McpToolInfo> =
            definitions.iter().map(Self::tool_schema_to_mcp).collect();

        Ok(serde_json::json!({
            "tools": mcp_tools,
        }))
    }

    /// Handle tools/call request.
    ///
    /// Dispatches the tool call through the shared tool registry.
    async fn handle_tools_call(&self, params: Value) -> Result<Value, McpError> {
        if !self.capability_policy.allow_tool_invoke {
            return Err(McpError::Forbidden(
                "tool invocation is not allowed by capability policy".to_string(),
            ));
        }
        let tool_name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing tool name".to_string()))?;

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        debug!("MCP tools/call: {} with args: {}", tool_name, arguments);

        // Dispatch through the tool registry
        let result = self
            .tool_registry
            .dispatch_async(tool_name, arguments)
            .await;

        // Parse the result back to check for errors
        let is_error = result.starts_with("{\"error\"");

        let content = vec![serde_json::json!({
            "type": "text",
            "text": result,
        })];

        Ok(serde_json::json!({
            "content": content,
            "isError": is_error,
        }))
    }

    /// Handle resources/list request.
    async fn handle_resources_list(&self, _params: Value) -> Result<Value, McpError> {
        let resources = &self.resources;
        Ok(serde_json::json!({
            "resources": resources,
        }))
    }

    /// Handle resources/read request.
    async fn handle_resources_read(&self, params: Value) -> Result<Value, McpError> {
        if !self.capability_policy.allow_resource_read {
            return Err(McpError::Forbidden(
                "resource read is not allowed by capability policy".to_string(),
            ));
        }
        let uri = params
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing resource uri".to_string()))?;

        // Find the resource
        let resource = self
            .resources
            .iter()
            .find(|r| r.uri == uri)
            .ok_or_else(|| McpError::ResourceNotFound(uri.to_string()))?;

        if let Some(contents) = self.resource_contents.get(uri) {
            return Ok(serde_json::json!({ "contents": contents }));
        }

        let contents = self.read_dynamic_resource(resource).await?;
        Ok(serde_json::json!({
            "contents": contents
        }))
    }

    async fn read_dynamic_resource(&self, resource: &ResourceInfo) -> Result<Vec<Value>, McpError> {
        let uri = resource.uri.as_str();
        if let Some(path) = uri.strip_prefix("file://") {
            let text = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| McpError::Io(e.to_string()))?;
            return Ok(vec![serde_json::json!({
                "uri": resource.uri,
                "mimeType": resource
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "text/plain".to_string()),
                "text": text,
            })]);
        }

        if uri.starts_with("http://") || uri.starts_with("https://") {
            let resp = reqwest::Client::new()
                .get(uri)
                .send()
                .await
                .map_err(|e| McpError::ConnectionError(e.to_string()))?;
            if !resp.status().is_success() {
                return Err(McpError::Protocol {
                    code: resp.status().as_u16() as i64,
                    message: format!("resource fetch failed: {}", resp.status()),
                });
            }
            let mime = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .or_else(|| resource.mime_type.clone())
                .unwrap_or_else(|| "text/plain".to_string());
            let text = resp.text().await.map_err(|e| McpError::Protocol {
                code: -32603,
                message: e.to_string(),
            })?;
            return Ok(vec![serde_json::json!({
                "uri": resource.uri,
                "mimeType": mime,
                "text": text,
            })]);
        }

        Err(McpError::NotConfigured(format!(
            "No content loader registered for resource URI '{}'",
            resource.uri
        )))
    }

    /// Handle prompts/list request.
    async fn handle_prompts_list(&self, _params: Value) -> Result<Value, McpError> {
        let prompts = &self.prompts;
        Ok(serde_json::json!({
            "prompts": prompts,
        }))
    }

    /// Handle prompts/get request.
    async fn handle_prompts_get(&self, params: Value) -> Result<Value, McpError> {
        if !self.capability_policy.allow_prompt_read {
            return Err(McpError::Forbidden(
                "prompt read is not allowed by capability policy".to_string(),
            ));
        }
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing prompt name".to_string()))?;

        let prompt = self
            .prompts
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| McpError::ResourceNotFound(format!("prompt '{}' not found", name)))?;

        Ok(serde_json::json!({
            "description": prompt.description,
            "messages": [],
        }))
    }

    /// Start the MCP server on the given transport.
    ///
    /// The server will listen for incoming JSON-RPC messages and dispatch
    /// them to the appropriate handler.
    pub async fn start(&self, mut transport: Box<dyn McpTransport>) -> Result<(), McpError> {
        info!("Starting MCP server");

        // Start the transport
        transport.start().await?;

        loop {
            // Receive a message
            let message = match transport.receive().await {
                Ok(msg) => msg,
                Err(McpError::ConnectionClosed) => {
                    info!("MCP server connection closed");
                    break;
                }
                Err(e) => {
                    warn!("Error receiving message: {}", e);
                    // Send error response
                    let error_response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32603,
                            "message": e.to_string(),
                        },
                        "id": Value::Null,
                    });
                    if let Err(send_err) = transport.send(error_response).await {
                        warn!("Failed to send error response: {}", send_err);
                        break;
                    }
                    continue;
                }
            };

            // Extract method, params, and id
            let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let params = message
                .get("params")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let id = message.get("id").cloned();

            // Handle the request
            let result = self.handle_request(method, params).await;

            // Build the response
            let response = match result {
                Ok(value) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "result": value,
                    "id": id,
                }),
                Err(e) => {
                    let (code, message) = match &e {
                        McpError::MethodNotFound(m) => (-32601, format!("Method not found: {}", m)),
                        McpError::InvalidParams(msg) => (-32602, msg.clone()),
                        McpError::Forbidden(msg) => (-32600, format!("Forbidden: {}", msg)),
                        McpError::NotConfigured(msg) => {
                            (-32001, format!("Not configured: {}", msg))
                        }
                        McpError::Protocol { code, message } => (*code, message.clone()),
                        other => (-32603, other.to_string()),
                    };
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": code,
                            "message": message,
                        },
                        "id": id,
                    })
                }
            };

            // Send response (only for requests with an id, not for notifications)
            if id.is_some() {
                if let Err(e) = transport.send(response).await {
                    warn!("Failed to send response: {}", e);
                    break;
                }
            }
        }

        // Clean up
        transport.close().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{McpCapabilityPolicy, McpPromptInfo, McpServer};
    use crate::client::ResourceInfo;
    use hermes_tools::ToolRegistry;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn prompts_get_returns_not_found() {
        let server = McpServer::new(Arc::new(ToolRegistry::new()));
        let err = server
            .handle_request("prompts/get", json!({"name":"missing"}))
            .await
            .expect_err("missing prompt should fail");
        assert!(matches!(err, crate::McpError::ResourceNotFound(_)));
    }

    #[tokio::test]
    async fn prompts_get_forbidden_when_capability_denied() {
        let mut server = McpServer::new(Arc::new(ToolRegistry::new())).with_capability_policy(
            McpCapabilityPolicy {
                allow_tool_invoke: true,
                allow_prompt_read: false,
                allow_resource_read: true,
            },
        );
        server.add_prompt(McpPromptInfo {
            name: "hello".to_string(),
            description: Some("d".to_string()),
            arguments: None,
        });
        let err = server
            .handle_request("prompts/get", json!({"name":"hello"}))
            .await
            .expect_err("prompt read should be denied");
        assert!(matches!(err, crate::McpError::Forbidden(_)));
    }

    #[tokio::test]
    async fn resources_read_returns_registered_text_content() {
        let mut server = McpServer::new(Arc::new(ToolRegistry::new()));
        server.add_resource_text(
            ResourceInfo {
                uri: "memory://note".to_string(),
                name: "note".to_string(),
                description: Some("test note".to_string()),
                mime_type: Some("text/plain".to_string()),
            },
            "hello world",
        );

        let result = server
            .handle_request("resources/read", json!({"uri":"memory://note"}))
            .await
            .expect("resource read should succeed");

        let text = result
            .get("contents")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn resources_read_file_uri_loads_text() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), "file content").expect("write");
        let uri = format!("file://{}", temp.path().display());

        let mut server = McpServer::new(Arc::new(ToolRegistry::new()));
        server.add_resource(ResourceInfo {
            uri: uri.clone(),
            name: "file".to_string(),
            description: None,
            mime_type: Some("text/plain".to_string()),
        });

        let result = server
            .handle_request("resources/read", json!({"uri":uri}))
            .await
            .expect("resource read should succeed");
        let text = result
            .get("contents")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(text, "file content");
    }
}
