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
            ping_unsupported: false,
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
        self.ping_unsupported = false;

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
                parameters: mcp_input_schema_to_json_schema(t.input_schema),
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

    /// Cheaply exercise the MCP session so HTTP/SSE servers do not expire it idle.
    pub async fn keepalive_probe(&mut self) -> Result<(), McpError> {
        if !self.ping_unsupported {
            match tokio::time::timeout(
                Duration::from_secs(MCP_KEEPALIVE_PROBE_TIMEOUT_SECS),
                self.send_request("ping", serde_json::json!({})),
            )
            .await
            {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(err)) if matches!(err, McpError::MethodNotFound(_)) => {
                    if self.tools.is_empty() {
                        return Err(err);
                    }
                    self.ping_unsupported = true;
                    info!(
                        "MCP server does not implement optional ping; using tools/list for keepalive on this connection."
                    );
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(McpError::ConnectionError(format!(
                        "MCP keepalive ping timed out after {}s",
                        MCP_KEEPALIVE_PROBE_TIMEOUT_SECS
                    )));
                }
            }
        }

        tokio::time::timeout(
            Duration::from_secs(MCP_KEEPALIVE_PROBE_TIMEOUT_SECS),
            self.list_tools(),
        )
        .await
        .map_err(|_| {
            McpError::ConnectionError(format!(
                "MCP keepalive tools/list timed out after {}s",
                MCP_KEEPALIVE_PROBE_TIMEOUT_SECS
            ))
        })??;
        Ok(())
    }

    async fn reconnect_after_keepalive_failure(&mut self) -> Result<(), McpError> {
        let config = self.config.clone();
        let sampling_config = self.sampling_config.clone();
        let sampling_callback = self.sampling_callback.clone();
        if let Some(mut transport) = self.transport.take() {
            let _ = transport.close().await;
        }
        self.connected = false;
        self.connected_at = None;
        self.tools.clear();
        self.resources.clear();

        let mut replacement = McpClient::new(config);
        if let Some(config) = sampling_config {
            replacement.set_sampling_config(config);
        }
        if let Some(callback) = sampling_callback {
            replacement.set_sampling_callback(callback);
        }
        replacement.connect().await?;
        *self = replacement;
        Ok(())
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
            let issues = validate_mcp_server_config("stdio", &self.config);
            if !issues.is_empty() {
                return Err(McpError::Config(format!(
                    "MCP stdio server config rejected: {}",
                    issues.join("; ")
                )));
            }
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

type SharedMcpClient = Arc<tokio::sync::Mutex<McpClient>>;

struct RegisteredMcpToolHandler {
    client: SharedMcpClient,
    original_tool_name: String,
    schema: ToolSchema,
    available: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl ToolHandler for RegisteredMcpToolHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(ToolError::NotFound(self.schema.name.clone()));
        }

        let mut client = self.client.lock().await;
        let result = client
            .call_tool(&self.original_tool_name, params)
            .await
            .map_err(|err| {
                ToolError::ExecutionFailed(format!(
                    "MCP tool '{}' failed: {}",
                    self.original_tool_name, err
                ))
            })?;
        Ok(match result {
            Value::String(text) => text,
            other => other.to_string(),
        })
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}
