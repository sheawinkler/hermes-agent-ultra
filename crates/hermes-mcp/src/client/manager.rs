struct ManagedMcpClient {
    client: SharedMcpClient,
    config: McpServerConfig,
    transport_type: String,
    cached_tools: Vec<ToolSchema>,
    cached_resources: Vec<ResourceInfo>,
    registered_tools: Vec<String>,
    available: Arc<AtomicBool>,
    connected_at: Instant,
    keepalive_shutdown: Arc<AtomicBool>,
    keepalive_task: Option<tokio::task::JoinHandle<()>>,
}

fn mcp_keepalive_interval_duration(config: &McpServerConfig) -> Duration {
    Duration::from_secs(
        config
            .keepalive_interval
            .unwrap_or(DEFAULT_MCP_KEEPALIVE_INTERVAL_SECS)
            .max(MIN_MCP_KEEPALIVE_INTERVAL_SECS),
    )
}

fn spawn_keepalive_task(
    server_name: String,
    config: &McpServerConfig,
    client: SharedMcpClient,
    available: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.is_http() {
        return None;
    }
    let interval = mcp_keepalive_interval_duration(config);
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if shutdown.load(Ordering::SeqCst) || !available.load(Ordering::SeqCst) {
                break;
            }

            let probe = {
                let mut client = client.lock().await;
                client.keepalive_probe().await
            };
            if probe.is_ok() {
                continue;
            }

            let probe_err = probe.expect_err("probe failed");
            warn!(
                "MCP server '{}' keepalive failed ({}); reconnecting once",
                server_name, probe_err
            );
            let reconnect = {
                let mut client = client.lock().await;
                client.reconnect_after_keepalive_failure().await
            };
            if let Err(err) = reconnect {
                warn!(
                    "MCP server '{}' keepalive reconnect failed: {}",
                    server_name, err
                );
                available.store(false, Ordering::SeqCst);
                break;
            }
        }
    }))
}

fn make_managed_client(
    name: &str,
    client: McpClient,
    tool_registry: &ToolRegistry,
) -> ManagedMcpClient {
    let cached_tools = client.cached_tools().to_vec();
    let cached_resources = client.cached_resources().to_vec();
    let transport_type = transport_type_for_config(&client.config).to_string();
    let config = client.config.clone();
    let shared_client = Arc::new(tokio::sync::Mutex::new(client));
    let available = Arc::new(AtomicBool::new(true));
    let registered_tools = register_mcp_tools_in_registry(
        tool_registry,
        name,
        Arc::clone(&shared_client),
        &cached_tools,
        Arc::clone(&available),
    );
    let keepalive_shutdown = Arc::new(AtomicBool::new(false));
    let keepalive_task = spawn_keepalive_task(
        name.to_string(),
        &config,
        Arc::clone(&shared_client),
        Arc::clone(&available),
        Arc::clone(&keepalive_shutdown),
    );
    ManagedMcpClient {
        client: shared_client,
        config,
        transport_type,
        cached_tools,
        cached_resources,
        registered_tools,
        available,
        connected_at: Instant::now(),
        keepalive_shutdown,
        keepalive_task,
    }
}

fn stop_keepalive_task(managed: &mut ManagedMcpClient) {
    managed.keepalive_shutdown.store(true, Ordering::SeqCst);
    if let Some(task) = managed.keepalive_task.take() {
        task.abort();
    }
}

fn register_mcp_tools_in_registry(
    registry: &ToolRegistry,
    server_name: &str,
    client: SharedMcpClient,
    tools: &[ToolSchema],
    available: Arc<AtomicBool>,
) -> Vec<String> {
    let toolset_name = mcp_toolset_name(server_name);
    let safe_server_name = sanitize_mcp_name_component(server_name);
    registry.register_toolset_alias(server_name, &toolset_name);
    if safe_server_name != server_name {
        registry.register_toolset_alias(&safe_server_name, &toolset_name);
    }

    let mut seen = HashSet::new();
    let mut registered = Vec::new();
    for tool in tools {
        let registered_name = mcp_registered_tool_name(server_name, &tool.name);
        if !seen.insert(registered_name.clone()) {
            warn!(
                "Skipping duplicate sanitized MCP tool '{}' from server '{}'",
                registered_name, server_name
            );
            continue;
        }

        let mut schema = tool.clone();
        schema.name = registered_name.clone();
        if schema.description.trim().is_empty() {
            schema.description = format!("MCP tool '{}' from server '{}'", tool.name, server_name);
        }
        let handler = Arc::new(RegisteredMcpToolHandler {
            client: Arc::clone(&client),
            original_tool_name: tool.name.clone(),
            schema: schema.clone(),
            available: Arc::clone(&available),
        });
        registry.register(
            &registered_name,
            &toolset_name,
            schema,
            handler,
            {
                let available = Arc::clone(&available);
                Arc::new(move || available.load(Ordering::SeqCst))
            },
            Vec::new(),
            true,
            format!("MCP tool '{}' from server '{}'", tool.name, server_name),
            "mcp",
            None,
        );
        registered.push(registered_name);
    }
    registered
}

fn deregister_mcp_tools(registry: &ToolRegistry, tool_names: Vec<String>) {
    for tool_name in tool_names {
        registry.deregister(&tool_name);
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
    clients: HashMap<String, ManagedMcpClient>,
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
        let issues = validate_mcp_server_config(name, &config);
        if !issues.is_empty() {
            return Err(McpError::Config(format!(
                "MCP server config rejected: {}",
                issues.join("; ")
            )));
        }
        if self.clients.contains_key(name) {
            self.disconnect(name).await?;
        }

        let mut client = McpClient::new(config);
        if let Some(config) = self.sampling_config.clone() {
            client.set_sampling_config(config);
        }
        if let Some(callback) = self.sampling_callback.clone() {
            client.set_sampling_callback(callback);
        }
        client.connect().await?;

        debug!(
            "Discovered {} tools from server '{}'",
            client.cached_tools().len(),
            name
        );

        self.clients.insert(
            name.to_string(),
            make_managed_client(name, client, self.tool_registry.as_ref()),
        );
        Ok(())
    }

    #[cfg(test)]
    async fn connect_with_transport_for_test(
        &mut self,
        name: &str,
        config: McpServerConfig,
        transport: Box<dyn McpTransport>,
    ) -> Result<(), McpError> {
        if self.clients.contains_key(name) {
            self.disconnect(name).await?;
        }
        let mut client = McpClient::new(config);
        client.finish_connect_with_transport(transport).await?;
        self.clients.insert(
            name.to_string(),
            make_managed_client(name, client, self.tool_registry.as_ref()),
        );
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
                        connected: existing.available.load(Ordering::SeqCst),
                        transport_type: existing.transport_type.clone(),
                        tool_count: existing.cached_tools.len(),
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
                        self.clients.insert(
                            report.name.clone(),
                            make_managed_client(&report.name, client, self.tool_registry.as_ref()),
                        );
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
        if let Some(mut managed) = self.clients.remove(name) {
            info!("Disconnecting from MCP server: {}", name);
            stop_keepalive_task(&mut managed);
            managed.available.store(false, Ordering::SeqCst);
            deregister_mcp_tools(self.tool_registry.as_ref(), managed.registered_tools);
            let mut client = managed.client.lock().await;
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
            .is_some_and(|c| c.available.load(Ordering::SeqCst))
    }

    /// Get the list of connected server names.
    pub fn connected_servers(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// Discover (or re-discover) tools on a connected server.
    pub async fn discover_tools(&mut self, server_name: &str) -> Result<Vec<ToolSchema>, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let tools = {
            let mut client = managed.client.lock().await;
            client.list_tools().await?
        };
        deregister_mcp_tools(
            self.tool_registry.as_ref(),
            std::mem::take(&mut managed.registered_tools),
        );
        managed.cached_tools = tools.clone();
        managed.registered_tools = register_mcp_tools_in_registry(
            self.tool_registry.as_ref(),
            server_name,
            Arc::clone(&managed.client),
            &tools,
            Arc::clone(&managed.available),
        );
        Ok(tools)
    }

    /// Call a tool on a connected MCP server.
    pub async fn call_tool(
        &mut self,
        server_name: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<Value, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;

        let reconnect_config: McpServerConfig = {
            let mut client = managed.client.lock().await;
            match client.call_tool(tool_name, args.clone()).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    if is_stale_transport_error(&err) {
                        warn!(
                            "MCP stale transport detected on '{}' ({}); reconnecting once",
                            server_name, err
                        );
                        managed.config.clone()
                    } else {
                        return Err(err);
                    }
                }
            }
        };

        let mut new_client = McpClient::new(reconnect_config.clone());
        if let Some(config) = self.sampling_config.clone() {
            new_client.set_sampling_config(config);
        }
        if let Some(callback) = self.sampling_callback.clone() {
            new_client.set_sampling_callback(callback);
        }
        new_client.connect().await?;
        let tools = new_client.cached_tools().to_vec();
        let resources = new_client.cached_resources().to_vec();
        {
            let mut client = managed.client.lock().await;
            *client = new_client;
        }
        deregister_mcp_tools(
            self.tool_registry.as_ref(),
            std::mem::take(&mut managed.registered_tools),
        );
        managed.config = reconnect_config;
        managed.cached_tools = tools.clone();
        managed.cached_resources = resources;
        managed.connected_at = Instant::now();
        managed.available.store(true, Ordering::SeqCst);
        managed.registered_tools = register_mcp_tools_in_registry(
            self.tool_registry.as_ref(),
            server_name,
            Arc::clone(&managed.client),
            &tools,
            Arc::clone(&managed.available),
        );

        let mut client = managed.client.lock().await;
        client.call_tool(tool_name, args).await
    }

    /// List resources available on a connected server.
    pub async fn list_resources(
        &mut self,
        server_name: &str,
    ) -> Result<Vec<ResourceInfo>, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let resources = {
            let mut client = managed.client.lock().await;
            client.list_resources().await?
        };
        managed.cached_resources = resources.clone();
        Ok(resources)
    }

    /// Read a resource from a connected server.
    pub async fn read_resource(&mut self, server_name: &str, uri: &str) -> Result<Value, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let mut client = managed.client.lock().await;
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
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let tools = {
            let mut client = managed.client.lock().await;
            client.list_tools().await?
        };
        debug!(
            "Re-discovered {} tools from server '{}'",
            tools.len(),
            server_name
        );
        deregister_mcp_tools(
            self.tool_registry.as_ref(),
            std::mem::take(&mut managed.registered_tools),
        );
        managed.cached_tools = tools.clone();
        managed.registered_tools = register_mcp_tools_in_registry(
            self.tool_registry.as_ref(),
            server_name,
            Arc::clone(&managed.client),
            &tools,
            Arc::clone(&managed.available),
        );
        Ok(tools)
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
            if let Ok(mut client) = client.client.try_lock() {
                client.set_sampling_config(config.clone());
            }
        }
    }

    /// Set the sampling callback for all connected and future clients.
    pub fn set_sampling_callback(&mut self, callback: LlmCallback) {
        self.sampling_callback = Some(callback.clone());
        for client in self.clients.values_mut() {
            if let Ok(mut client) = client.client.try_lock() {
                client.set_sampling_callback(callback.clone());
            }
        }
    }

    /// Return sampling audit counters for a connected server.
    pub fn sampling_metrics(&self, server_name: &str) -> Option<SamplingMetrics> {
        self.clients
            .get(server_name)
            .and_then(|client| client.client.try_lock().ok())
            .map(|client| client.sampling_metrics().clone())
    }

    // -----------------------------------------------------------------------
    // Prompts
    // -----------------------------------------------------------------------

    /// List prompts available on a connected server.
    pub async fn list_prompts(&mut self, server_name: &str) -> Result<Vec<PromptInfo>, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let mut client = managed.client.lock().await;
        client.list_prompts().await
    }

    /// Get a prompt from a connected server.
    pub async fn get_prompt(
        &mut self,
        server_name: &str,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let managed = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.to_string()))?;
        let mut client = managed.client.lock().await;
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
                let status = McpServerStatus {
                    name: name.clone(),
                    connected: client.available.load(Ordering::SeqCst),
                    tool_count: client.cached_tools.len(),
                    resource_count: client.cached_resources.len(),
                    transport_type: client.transport_type.clone(),
                    uptime_secs: Some(client.connected_at.elapsed().as_secs()),
                };
                (name.clone(), status)
            })
            .collect()
    }

    /// Probe a connected MCP server to check reachability and discover capabilities.
    pub async fn probe_server(&mut self, name: &str) -> Result<McpProbeResult, McpError> {
        let managed = self
            .clients
            .get_mut(name)
            .ok_or_else(|| McpError::ServerNotFound(name.to_string()))?;

        let start = Instant::now();
        let tools_result = {
            let mut client = managed.client.lock().await;
            client.list_tools().await
        };
        let latency = start.elapsed();

        match tools_result {
            Ok(tools) => {
                let resources = {
                    let mut client = managed.client.lock().await;
                    client.list_resources().await.unwrap_or_default()
                };
                managed.cached_tools = tools.clone();
                managed.cached_resources = resources.clone();

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
