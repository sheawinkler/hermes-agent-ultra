fn acp_mcp_servers_from_params(p: Option<&serde_json::Map<String, Value>>) -> Vec<McpServerConfig> {
    let Some(value) = p.and_then(|p| p.get("mcpServers").or_else(|| p.get("mcp_servers"))) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        tracing::warn!("ACP mcp_servers parameter was not an array; ignoring");
        return Vec::new();
    };
    items
        .iter()
        .filter_map(
            |item| match serde_json::from_value::<McpServerConfig>(item.clone()) {
                Ok(server) => Some(server),
                Err(err) => {
                    tracing::warn!("Ignoring invalid ACP MCP server entry: {}", err);
                    None
                }
            },
        )
        .collect()
}

fn bearer_token_from_headers(headers: &[EnvVar]) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("authorization"))
        .map(|header| header.value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

fn acp_mcp_server_to_hermes_config(
    server: &McpServerConfig,
) -> Option<(String, HermesMcpServerConfig)> {
    match server {
        McpServerConfig::Stdio {
            name,
            command,
            args,
            env,
        } => {
            let name = sanitize_mcp_name_component(name.trim());
            if name.is_empty() || command.trim().is_empty() {
                return None;
            }
            let mut config = HermesMcpServerConfig::stdio(command.trim(), args.clone());
            for item in env {
                if !item.name.trim().is_empty() {
                    config = config.with_env(item.name.trim(), item.value.clone());
                }
            }
            Some((name, config))
        }
        McpServerConfig::Http {
            name,
            url,
            headers,
            keepalive_interval,
        }
        | McpServerConfig::Sse {
            name,
            url,
            headers,
            keepalive_interval,
        } => {
            let name = sanitize_mcp_name_component(name.trim());
            if name.is_empty() || url.trim().is_empty() {
                return None;
            }
            let mut config = HermesMcpServerConfig::http(url.trim());
            if let Some(seconds) = keepalive_interval {
                config = config.with_keepalive_interval(*seconds);
            }
            if let Some(token) = bearer_token_from_headers(headers) {
                config = config.with_auth(Arc::new(BearerTokenAuth::new(token)));
            }
            Some((name, config))
        }
    }
}

fn expand_acp_enabled_toolsets(
    toolsets: impl IntoIterator<Item = String>,
    mcp_server_names: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut expanded = Vec::new();
    for name in toolsets {
        let name = name.trim();
        if !name.is_empty() && !expanded.iter().any(|existing| existing == name) {
            expanded.push(name.to_string());
        }
    }
    if expanded.is_empty() {
        expanded.push("hermes-acp".to_string());
    }
    for server_name in mcp_server_names {
        let safe = sanitize_mcp_name_component(server_name.trim());
        if safe.is_empty() {
            continue;
        }
        let toolset_name = format!("mcp-{safe}");
        if !expanded.iter().any(|existing| existing == &toolset_name) {
            expanded.push(toolset_name);
        }
    }
    expanded
}

// ---------------------------------------------------------------------------
// HermesAcpHandler
// ---------------------------------------------------------------------------

/// Full ACP handler wrapping Hermes agent capabilities.
pub struct HermesAcpHandler {
    pub session_manager: Arc<SessionManager>,
    pub event_sink: Arc<EventSink>,
    pub permission_store: Arc<PermissionStore>,
    tool_registry: Arc<ToolRegistry>,
    mcp_manager: Arc<AsyncMutex<McpManager>>,
    version: String,
    prompt_executor: Option<Arc<dyn AcpPromptExecutor>>,
    auth_provider_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}
