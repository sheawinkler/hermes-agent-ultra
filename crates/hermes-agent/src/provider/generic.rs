/// A generic LLM provider that can be configured for any OpenAI-compatible API.
///
/// This is the primary provider used by the agent loop. It supports
/// OpenAI-compatible APIs via configuration.
#[derive(Debug, Clone)]
pub struct GenericProvider {
    /// Base URL for the API endpoint.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client.
    client: Arc<Mutex<Client>>,
    /// Optional total request timeout applied to newly-built clients.
    request_timeout: Option<Duration>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<Mutex<Instant>>,
    /// Optional custom headers to send with every request.
    pub extra_headers: Vec<(String, String)>,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool for key rotation.
    pub credential_pool: Option<Arc<CredentialPool>>,
    /// Optional OpenAI-compatible provider profile used for request shaping.
    pub provider_profile: Option<String>,
}

impl GenericProvider {
    /// Create a new generic provider.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let request_timeout = None;
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: Arc::new(Mutex::new(build_provider_http_client(request_timeout))),
            request_timeout,
            client_refreshed_at: Arc::new(Mutex::new(Instant::now())),
            extra_headers: Vec::new(),
            rate_limiter: None,
            credential_pool: None,
            provider_profile: None,
        }
    }

    /// Add a custom header to be sent with every request.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((key.into(), value.into()));
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set an optional total request timeout used by this provider and rebuilds.
    pub fn with_optional_request_timeout_seconds(mut self, seconds: Option<f64>) -> Self {
        self.request_timeout = request_timeout_duration(seconds);
        if let Ok(mut client) = self.client.lock() {
            *client = build_provider_http_client(self.request_timeout);
        }
        self
    }

    /// Set a total request timeout in seconds.
    pub fn with_request_timeout_seconds(self, seconds: f64) -> Self {
        self.with_optional_request_timeout_seconds(Some(seconds))
    }

    #[cfg(test)]
    pub(crate) fn configured_request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    /// Attach a Rust-native provider profile for request shaping.
    pub fn with_provider_profile(mut self, profile: impl Into<String>) -> Self {
        self.provider_profile =
            provider_profiles::canonical_provider_profile_id(&profile.into()).map(str::to_string);
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    /// Get the effective API key, using the credential pool if available.
    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    /// Check rate limits before making a request. Waits if needed.
    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!(
                    "Rate limited, waiting {:?} before next request",
                    wait_duration
                );
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    /// Update rate limit state from response headers.
    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    /// Inject optional runtime hints: reasoning effort, vision preprocessing,
    /// and service tier.
    fn apply_runtime_hints(
        &self,
        body: &mut Value,
        messages: &[Message],
        extra_body: Option<&Value>,
    ) {
        // Reasoning effort passthrough (`low|medium|high`) using extra_body.reasoning_effort.
        if let Some(eb) = extra_body
            .and_then(|v| v.get("reasoning_effort"))
            .and_then(|v| v.as_str())
        {
            body["reasoning_effort"] = serde_json::json!(eb);
        }

        // OpenAI service tier passthrough.
        if let Some(st) = extra_body
            .and_then(|v| v.get("service_tier"))
            .and_then(|v| v.as_str())
        {
            body["service_tier"] = serde_json::json!(st);
        }

        // Vision preprocessing: if user content contains local file-like paths,
        // add a hint field used by downstream adapters.
        let needs_vision_preprocess = messages.iter().any(|m| {
            m.content.as_ref().is_some_and(|c| {
                c.contains(".png") || c.contains(".jpg") || c.contains("data:image/")
            })
        });
        if needs_vision_preprocess {
            body["vision_preprocessed"] = serde_json::json!(true);
        }
    }

    fn apply_opencode_go_reasoning_controls(&self, body: &mut Value, effective_model: &str) {
        if !self
            .base_url
            .to_ascii_lowercase()
            .contains("opencode.ai/zen/go")
        {
            return;
        }

        let model = flat_model_name(effective_model);
        let is_kimi_k2 = model.starts_with("kimi-k2");
        let is_deepseek_thinking = (model.starts_with("deepseek-v")
            && !model.starts_with("deepseek-v3"))
            || model == "deepseek-reasoner";

        let reasoning = body.get("reasoning").cloned();
        let mut enabled = true;
        let mut effort = body
            .get("reasoning_effort")
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_ascii_lowercase());

        if let Some(reasoning_obj) = reasoning.as_ref().and_then(|value| value.as_object()) {
            enabled = reasoning_obj
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            if effort.is_none() {
                effort = reasoning_obj
                    .get("effort")
                    .and_then(|value| value.as_str())
                    .map(|value| value.trim().to_ascii_lowercase());
            }
        }

        if let Some(map) = body.as_object_mut() {
            map.remove("reasoning");
            map.remove("reasoning_effort");
        }

        if is_kimi_k2 {
            if reasoning.is_none() && effort.is_none() {
                return;
            }
            body["thinking"] =
                serde_json::json!({ "type": if enabled { "enabled" } else { "disabled" } });
            if enabled {
                if let Some(mapped) = opencode_go_kimi_reasoning_effort(effort.as_deref()) {
                    body["reasoning_effort"] = serde_json::json!(mapped);
                }
            }
            return;
        }

        if is_deepseek_thinking {
            body["thinking"] =
                serde_json::json!({ "type": if enabled { "enabled" } else { "disabled" } });
            if enabled {
                if let Some(mapped) = opencode_go_deepseek_reasoning_effort(effort.as_deref()) {
                    body["reasoning_effort"] = serde_json::json!(mapped);
                }
            }
        }
    }

    /// Force-close helper for future explicit TCP cleanup hooks.
    pub fn force_close_tcp_sockets(&self) {
        // reqwest handles connection pooling internally; dropping clones and relying
        // on idle timeout is currently sufficient for our runtime.
    }

    fn current_client(&self) -> Client {
        self.client
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| build_provider_http_client(self.request_timeout))
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding primary HTTP client: {}", reason);
        if let Ok(mut c) = self.client.lock() {
            *c = build_provider_http_client(self.request_timeout);
        }
        if let Ok(mut t) = self.client_refreshed_at.lock() {
            *t = Instant::now();
        }
    }

    async fn maybe_refresh_stale_client(&self, probe_url: &str) {
        const STALE_CLIENT_REFRESH_SECS: u64 = 300;
        let stale_after = Duration::from_secs(STALE_CLIENT_REFRESH_SECS);
        let should_refresh = self
            .client_refreshed_at
            .lock()
            .map(|t| t.elapsed() >= stale_after)
            .unwrap_or(false);
        if !should_refresh {
            return;
        }
        let probe_client = self.current_client();
        match probe_client
            .get(probe_url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(_) => {
                if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
            Err(e) => {
                if Self::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
        }
    }

    fn is_connection_recoverable(err: &reqwest::Error) -> bool {
        if err.is_connect() || err.is_timeout() || err.is_request() {
            return true;
        }
        let msg = err.to_string().to_lowercase();
        msg.contains("connection reset")
            || msg.contains("connection closed")
            || msg.contains("broken pipe")
            || msg.contains("pool")
            || msg.contains("eof")
    }

    fn should_sanitize_tool_calls(extra_body: Option<&Value>) -> bool {
        extra_body
            .and_then(|v| {
                v.get("strict_tool_calls")
                    .or_else(|| v.get("strict_api"))
                    .or_else(|| v.get("provider_strict"))
            })
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn is_local_request_control_key(key: &str) -> bool {
        matches!(key, "strict_tool_calls" | "strict_api" | "provider_strict")
            || provider_profiles::local_control_key_for_profile(None, key)
    }

    fn merge_extra_body_fields(body: &mut Value, extra_body: Option<&Value>) {
        let Some(Value::Object(map)) = extra_body else {
            return;
        };
        for (k, v) in map {
            if Self::is_local_request_control_key(k) {
                continue;
            }
            body[k] = v.clone();
        }
    }

    fn profile_for_extra_body<'a>(&'a self, extra_body: Option<&'a Value>) -> Option<&'a str> {
        extra_body
            .and_then(|value| value.get("provider_profile"))
            .and_then(Value::as_str)
            .and_then(provider_profiles::canonical_provider_profile_id)
            .or(self.provider_profile.as_deref())
    }

    fn sanitize_messages_for_api(
        messages: &[Message],
        enabled: bool,
        effective_model: &str,
        profile: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Value {
        let model_supports_vision =
            Self::supports_multimodal_tool_results(profile, effective_model, extra_body);
        let mut out = Vec::with_capacity(messages.len());
        for msg in messages {
            let mut api_msg = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(parts) = api_msg
                .get("content")
                .and_then(|v| v.as_str())
                .and_then(parse_acp_multimodal_parts)
            {
                let is_tool_message = api_msg
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| role.eq_ignore_ascii_case("tool"));
                let supports_tool_multimodal = profile
                    .map(provider_profiles::supports_vision_tool_messages)
                    .unwrap_or(true);
                api_msg["content"] =
                    if model_supports_vision && (!is_tool_message || supports_tool_multimodal) {
                        Value::Array(parts)
                    } else {
                        Value::String(flatten_multimodal_parts_text(&parts))
                    };
            }
            if !enabled {
                out.push(api_msg);
                continue;
            }
            if let Some(tool_calls) = api_msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for tc in tool_calls.iter_mut() {
                    if let Some(obj) = tc.as_object_mut() {
                        let id = obj.get("id").cloned();
                        let function = obj.get("function").cloned().or_else(|| {
                            let name = obj.get("name").and_then(|v| v.as_str())?.to_string();
                            let args_raw = obj
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| Value::String("{}".to_string()));
                            let (args, _) = arguments_value_to_string(Some(&args_raw));
                            Some(serde_json::json!({
                                "name": name,
                                "arguments": args,
                            }))
                        });
                        let mut stripped = serde_json::Map::new();
                        if let Some(v) = id {
                            stripped.insert("id".to_string(), v);
                        }
                        stripped.insert(
                            "type".to_string(),
                            obj.get("type")
                                .cloned()
                                .unwrap_or_else(|| Value::String("function".to_string())),
                        );
                        if let Some(v) = function {
                            stripped.insert("function".to_string(), v);
                        }
                        *obj = stripped;
                    }
                }
            }
            out.push(api_msg);
        }
        Value::Array(out)
    }

    fn supports_multimodal_tool_results(
        profile: Option<&str>,
        effective_model: &str,
        extra_body: Option<&Value>,
    ) -> bool {
        if let Some(value) = extra_body
            .and_then(|body| body.get("supports_vision"))
            .and_then(Value::as_bool)
        {
            return value;
        }
        profile.is_some_and(provider_profiles::supports_vision) || supports_vision(effective_model)
    }

    fn format_tools_for_openai_api(tools: &[ToolSchema]) -> Value {
        let formatted = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
        hermes_core::sanitize_tool_schemas(Some(&formatted)).unwrap_or(formatted)
    }

    fn chat_request_body(&self, request: ChatRequestParams<'_>) -> Value {
        let ChatRequestParams {
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model,
            extra_body,
            stream,
        } = request;
        let profile = self.profile_for_extra_body(extra_body);
        let native_gemini = provider_profiles::is_native_gemini_base_url(&self.base_url);
        let strict_tool_sanitize = Self::should_sanitize_tool_calls(extra_body);
        let request_extra_body = if native_gemini {
            provider_profiles::clean_extra_body_for_native_gemini(extra_body)
        } else {
            provider_profiles::clean_extra_body_for_profile(profile, extra_body)
        };
        let mut api_messages = Self::sanitize_messages_for_api(
            messages,
            strict_tool_sanitize,
            effective_model,
            profile,
            extra_body,
        );
        provider_profiles::normalize_messages_for_profile(profile, &mut api_messages);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });
        if stream {
            body["stream"] = Value::Bool(true);
        }

        if let Some(mt) =
            max_tokens.or_else(|| profile.and_then(provider_profiles::default_max_tokens))
        {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if !profile.is_some_and(provider_profiles::omit_temperature) {
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
        }
        if !tools.is_empty() {
            body["tools"] =
                format_tools_for_openai_api_with_model(tools, effective_model, &self.base_url);
        }
        Self::merge_extra_body_fields(&mut body, request_extra_body.as_ref());
        self.apply_runtime_hints(&mut body, messages, request_extra_body.as_ref());
        if !native_gemini {
            provider_profiles::apply_profile_to_body(
                profile,
                &mut body,
                effective_model,
                &self.base_url,
                extra_body,
            );
        }
        self.apply_opencode_go_reasoning_controls(&mut body, effective_model);
        if body
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(is_openai_dynamic_model_alias)
            && !is_openai_dynamic_model_alias(effective_model)
        {
            body["model"] = serde_json::json!(effective_model);
        }
        body
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let mut req = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(body);
        if api_key.trim() != "local-no-key" {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }
        if provider_profiles::is_kimi_code_base_url(&self.base_url) {
            req = req.header("User-Agent", provider_profiles::KIMI_CODE_USER_AGENT);
        }
        for (key, value) in &self.extra_headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    fn capture_nous_credits_headers(&self, headers: &reqwest::header::HeaderMap) {
        let _ = hermes_core::credits::capture_nous_credits_from_pairs(headers.iter().filter_map(
            |(key, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (key.as_str().to_string(), value.to_string()))
            },
        ));
    }

    async fn send_with_dead_connection_recovery(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AgentError> {
        self.maybe_refresh_stale_client(url).await;
        let client = self.current_client();
        match self.build_request(&client, url, api_key, body).send().await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if !Self::is_connection_recoverable(&e) {
                    return Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
                }
                self.refresh_client(&format!("recoverable transport error: {e}"));
                let retry_client = self.current_client();
                self.build_request(&retry_client, url, api_key, body)
                    .send()
                    .await
                    .map_err(|e2| {
                        AgentError::LlmApi(format!(
                            "HTTP request failed after reconnect retry: {e2}"
                        ))
                    })
            }
        }
    }
}

fn flat_model_name(model: &str) -> String {
    let normalized = model.trim().to_ascii_lowercase();
    let parts = normalized
        .split(['/', ':'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for part in parts.iter().rev() {
        if part.starts_with("kimi-k2")
            || part.starts_with("deepseek-v")
            || *part == "deepseek-reasoner"
        {
            return (*part).to_string();
        }
    }

    parts
        .last()
        .copied()
        .unwrap_or(normalized.as_str())
        .to_string()
}

fn opencode_go_kimi_reasoning_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("high"),
        _ => None,
    }
}

fn opencode_go_deepseek_reasoning_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("max"),
        _ => None,
    }
}

fn is_moonshot_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    flat_model_name(&lower).starts_with("kimi-k2") || lower.contains("moonshotai/")
}

fn format_tools_for_openai_api_with_model(
    tools: &[ToolSchema],
    effective_model: &str,
    base_url: &str,
) -> Value {
    let mut formatted = GenericProvider::format_tools_for_openai_api(tools);
    if is_moonshot_model(effective_model)
        || AnthropicProvider::is_kimi_coding_endpoint(Some(base_url))
    {
        sanitize_moonshot_tools_value(&mut formatted);
    }
    formatted
}

fn sanitize_moonshot_tools_value(tools: &mut Value) {
    let Some(items) = tools.as_array_mut() else {
        return;
    };
    for tool in items {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let params = function
            .remove("parameters")
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        function.insert(
            "parameters".to_string(),
            sanitize_moonshot_tool_parameters(&params),
        );
    }
}

fn sanitize_moonshot_tool_parameters(params: &Value) -> Value {
    let mut root = match params.as_object() {
        Some(obj) => Value::Object(obj.clone()),
        None => serde_json::json!({"type": "object", "properties": {}}),
    };
    sanitize_moonshot_schema_node(&mut root, true);
    root
}

fn sanitize_moonshot_schema_node(node: &mut Value, top_level: bool) {
    let Some(obj) = node.as_object_mut() else {
        if top_level {
            *node = serde_json::json!({"type": "object", "properties": {}});
        }
        return;
    };

    let has_ref = obj.contains_key("$ref");

    if let Some(any_of) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in any_of.iter_mut() {
            sanitize_moonshot_schema_node(branch, false);
        }
        any_of.retain(|branch| {
            branch
                .get("type")
                .and_then(Value::as_str)
                .is_none_or(|kind| kind != "null")
        });
    }

    if obj.contains_key("anyOf") {
        let non_null = obj
            .get("anyOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if non_null.len() == 1 {
            obj.remove("anyOf");
            if let Some(branch_obj) = non_null
                .into_iter()
                .next()
                .and_then(|v| v.as_object().cloned())
            {
                for (key, value) in branch_obj {
                    obj.insert(key, value);
                }
            }
        } else {
            obj.remove("type");
        }
    }

    obj.remove("nullable");

    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for value in props.values_mut() {
            sanitize_moonshot_schema_node(value, false);
        }
    }

    if let Some(items) = obj.get_mut("items") {
        sanitize_moonshot_schema_node(items, false);
    }

    if let Some(any_of) = obj.get_mut("anyOf").and_then(Value::as_array_mut) {
        for branch in any_of {
            sanitize_moonshot_schema_node(branch, false);
        }
    }

    clean_moonshot_enum(obj);

    if top_level {
        obj.insert("type".to_string(), Value::String("object".to_string()));
        if !obj.get("properties").is_some_and(Value::is_object) {
            obj.insert(
                "properties".to_string(),
                Value::Object(serde_json::Map::new()),
            );
        }
        return;
    }

    if !has_ref && !obj.contains_key("type") && !obj.contains_key("anyOf") {
        let inferred = infer_moonshot_schema_type(obj);
        obj.insert("type".to_string(), Value::String(inferred.to_string()));
        clean_moonshot_enum(obj);
    }
}

fn clean_moonshot_enum(obj: &mut serde_json::Map<String, Value>) {
    let scalar = matches!(
        obj.get("type").and_then(Value::as_str),
        Some("string" | "integer" | "number" | "boolean")
    );
    if !scalar {
        return;
    }
    let Some(values) = obj.get_mut("enum").and_then(Value::as_array_mut) else {
        return;
    };
    values.retain(|value| !value.is_null() && !matches!(value.as_str(), Some("")));
    if values.is_empty() {
        obj.remove("enum");
    }
}

fn infer_moonshot_schema_type(obj: &serde_json::Map<String, Value>) -> &'static str {
    if obj.get("properties").is_some_and(Value::is_object) {
        return "object";
    }
    if obj.contains_key("items") {
        return "array";
    }
    if let Some(values) = obj.get("enum").and_then(Value::as_array) {
        if let Some(first) = values
            .iter()
            .find(|value| !value.is_null() && !matches!(value.as_str(), Some("")))
        {
            if first.is_boolean() {
                return "boolean";
            }
            if first.is_i64() || first.is_u64() {
                return "integer";
            }
            if first.is_number() {
                return "number";
            }
        }
    }
    "string"
}

#[async_trait]
impl LlmProvider for GenericProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.check_rate_limit().await;

        let api_key = self.effective_api_key();
        let effective_model = resolve_openai_compatible_dynamic_wire_model(
            model.unwrap_or(&self.model),
            &api_key,
            &self.base_url,
        );
        let body = self.chat_request_body(ChatRequestParams {
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model: effective_model.as_str(),
            extra_body,
            stream: false,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());
        self.capture_nous_credits_headers(resp.headers());

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }

        let resp_json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Failed to parse response: {e}")))?;

        parse_openai_response(&resp_json)
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(|s| s.to_string());
        let extra_body = extra_body.cloned();

        async_stream::stream! {
            provider.check_rate_limit().await;

            let api_key = provider.effective_api_key();
            let effective_model = resolve_openai_compatible_dynamic_wire_model(
                model.as_deref().unwrap_or(&provider.model),
                &api_key,
                &provider.base_url,
            );
            let mut body = provider.chat_request_body(ChatRequestParams {
                messages: &messages,
                tools: &tools,
                max_tokens,
                temperature,
                effective_model: effective_model.as_str(),
                extra_body: extra_body.as_ref(),
                stream: true,
            });
            // Request usage in the final streaming chunk
            body["stream_options"] = serde_json::json!({"include_usage": true});

            let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));

            let resp = match provider
                .send_with_dead_connection_recovery(&url, &api_key, &body)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());
            provider.capture_nous_credits_headers(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            // Read the SSE byte stream line by line
            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

                // Process complete SSE events (separated by double newlines)
                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                // Stream finished
                                return;
                            }
                            match serde_json::from_str::<Value>(data) {
                                Ok(json) => {
                                    if let Some(chunk) = parse_sse_chunk(&json) {
                                        yield Ok(chunk);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse SSE data: {e}");
                                }
                            }
                        }
                    }
                }
            }

            // Process any remaining data in the buffer
            if !buffer.trim().is_empty() {
                for line in buffer.lines() {
                    let line = line.trim();
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            if let Some(chunk) = parse_sse_chunk(&json) {
                                yield Ok(chunk);
                            }
                        }
                    }
                }
            }
        }
        .boxed()
    }
}

