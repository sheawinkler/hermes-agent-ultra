//! LLM provider implementations.
//!
//! Provides concrete implementations of the `LlmProvider` trait for
//! OpenAI, Anthropic, and OpenRouter APIs.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermes_intelligence::anthropic_adapter::get_anthropic_max_output;

use hermes_core::{
    AgentError, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

use crate::credential_pool::CredentialPool;
use crate::rate_limit::RateLimitTracker;

// ---------------------------------------------------------------------------
// GenericProvider — a flexible, config-driven provider
// ---------------------------------------------------------------------------

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
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<Mutex<Instant>>,
    /// Optional custom headers to send with every request.
    pub extra_headers: Vec<(String, String)>,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool for key rotation.
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl GenericProvider {
    /// Create a new generic provider.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: Arc::new(Mutex::new(Client::new())),
            client_refreshed_at: Arc::new(Mutex::new(Instant::now())),
            extra_headers: Vec::new(),
            rate_limiter: None,
            credential_pool: None,
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
            m.content
                .as_deref()
                .map(|c| c.contains(".png") || c.contains(".jpg") || c.contains("data:image/"))
                .unwrap_or(false)
        });
        if needs_vision_preprocess {
            body["vision_preprocessed"] = serde_json::json!(true);
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
            .unwrap_or_else(|_| Client::new())
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding primary HTTP client: {}", reason);
        if let Ok(mut c) = self.client.lock() {
            *c = Client::new();
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

    fn sanitize_messages_for_strict_api(messages: &[Message], enabled: bool) -> Value {
        if !enabled {
            return serde_json::to_value(messages).unwrap_or_else(|_| serde_json::json!([]));
        }
        let mut out = Vec::with_capacity(messages.len());
        for msg in messages {
            let mut api_msg = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(tool_calls) = api_msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for tc in tool_calls.iter_mut() {
                    if let Some(obj) = tc.as_object_mut() {
                        let id = obj.get("id").cloned();
                        let function = obj.get("function").cloned();
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

    fn format_tools_for_openai_api(tools: &[ToolSchema]) -> Value {
        Value::Array(
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
        )
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
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(body);
        for (key, value) in &self.extra_headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
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

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let strict_tool_sanitize = Self::should_sanitize_tool_calls(extra_body);
        let api_messages = Self::sanitize_messages_for_strict_api(messages, strict_tool_sanitize);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });

        if let Some(mt) = max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = Self::format_tools_for_openai_api(tools);
        }
        if let Some(eb) = extra_body {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }
        self.apply_runtime_hints(&mut body, messages, extra_body);

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

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

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let strict_tool_sanitize = GenericProvider::should_sanitize_tool_calls(extra_body.as_ref());
            let api_messages =
                GenericProvider::sanitize_messages_for_strict_api(&messages, strict_tool_sanitize);

            let mut body = serde_json::json!({
                "model": effective_model,
                "messages": api_messages,
                "stream": true,
            });

            if let Some(mt) = max_tokens {
                body["max_tokens"] = serde_json::json!(mt);
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = GenericProvider::format_tools_for_openai_api(&tools);
            }
            if let Some(ref eb) = extra_body {
                if let Value::Object(map) = eb {
                    for (k, v) in map {
                        body[k] = v.clone();
                    }
                }
            }
            provider.apply_runtime_hints(&mut body, &messages, extra_body.as_ref());
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

// ---------------------------------------------------------------------------
// OpenAiProvider
// ---------------------------------------------------------------------------

/// OpenAI API provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    inner: GenericProvider,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.openai.com/v1", api_key, "gpt-4o"),
        }
    }

    /// Use a custom base URL (e.g., for Azure OpenAI).
    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(base_url),
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
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
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// Anthropic API provider with native Messages API support.
///
/// Uses Anthropic's own message format rather than OpenAI-compatible format:
/// - System message goes in `system` parameter, not in messages array
/// - Uses `x-api-key` header instead of `Authorization: Bearer`
/// - Content blocks use array format with typed blocks
/// - Tool use returns `type: "tool_use"` content blocks
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// Base URL for the Anthropic API.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// HTTP client.
    client: Arc<Mutex<Client>>,
    /// Last time we rebuilt the client transport.
    client_refreshed_at: Arc<Mutex<Instant>>,
    /// Anthropic API version header.
    pub api_version: String,
    /// Optional rate limit tracker.
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    /// Optional credential pool.
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: api_key.into(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            client: Arc::new(Mutex::new(Client::new())),
            client_refreshed_at: Arc::new(Mutex::new(Instant::now())),
            api_version: "2023-06-01".to_string(),
            rate_limiter: None,
            credential_pool: None,
        }
    }

    /// Set the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Attach a rate limit tracker.
    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

    /// Attach a credential pool.
    pub fn with_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.credential_pool = Some(pool);
        self
    }

    fn effective_api_key(&self) -> String {
        if let Some(ref pool) = self.credential_pool {
            pool.get_key()
        } else {
            self.api_key.clone()
        }
    }

    async fn check_rate_limit(&self) {
        if let Some(ref tracker) = self.rate_limiter {
            if let Some(wait_duration) = tracker.should_wait() {
                tracing::info!("Rate limited, waiting {:?}", wait_duration);
                tokio::time::sleep(wait_duration).await;
            }
        }
    }

    fn update_rate_limit(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(ref tracker) = self.rate_limiter {
            tracker.update_from_headers(headers);
        }
    }

    fn current_client(&self) -> Client {
        self.client
            .lock()
            .map(|c| c.clone())
            .unwrap_or_else(|_| Client::new())
    }

    fn refresh_client(&self, reason: &str) {
        tracing::warn!("rebuilding anthropic HTTP client: {}", reason);
        if let Ok(mut c) = self.client.lock() {
            *c = Client::new();
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
                if GenericProvider::is_connection_recoverable(&e) {
                    self.refresh_client(&format!("stale connection probe failed: {e}"));
                } else if let Ok(mut t) = self.client_refreshed_at.lock() {
                    *t = Instant::now();
                }
            }
        }
    }

    fn build_request(
        &self,
        client: &Client,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.api_version)
            .header("Content-Type", "application/json")
            .json(body)
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
                if !GenericProvider::is_connection_recoverable(&e) {
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

    fn is_kimi_coding_endpoint(base_url: Option<&str>) -> bool {
        let Some(url) = base_url else {
            return false;
        };
        let lower = url.to_lowercase();
        lower.contains("api.kimi.com")
            || lower.contains("moonshot.ai")
            || lower.contains("moonshot.cn")
    }

    /// Resolve Anthropic Messages `max_tokens` to a strictly positive value.
    ///
    /// `Some(0)` is treated as invalid and falls back to the model ceiling,
    /// preventing avoidable 400s from upstream APIs.
    fn resolve_messages_max_tokens(requested: Option<u32>, model: &str) -> u32 {
        if let Some(value) = requested.filter(|v| *v > 0) {
            return value;
        }
        get_anthropic_max_output(model).max(1)
    }

    /// Convert internal messages to Anthropic format, extracting system message.
    fn convert_messages(
        messages: &[Message],
        base_url: Option<&str>,
    ) -> (Option<String>, Vec<Value>) {
        let mut system_text: Option<String> = None;
        let mut anthropic_messages: Vec<Value> = Vec::new();
        let is_kimi_endpoint = Self::is_kimi_coding_endpoint(base_url);

        for msg in messages {
            match msg.role {
                MessageRole::System => {
                    // Anthropic: system goes in a separate `system` parameter
                    let content = msg.content.as_deref().unwrap_or("");
                    system_text = Some(match system_text {
                        Some(existing) => format!("{existing}\n\n{content}"),
                        None => content.to_string(),
                    });
                }
                MessageRole::User => {
                    let mut content_blocks = Vec::new();
                    if let Some(ref text) = msg.content {
                        let mut block = serde_json::json!({"type": "text", "text": text});
                        if let Some(ref cc) = msg.cache_control {
                            block["cache_control"] = serde_json::json!({"type": format!("{:?}", cc.cache_type).to_lowercase()});
                        }
                        content_blocks.push(block);
                    }
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
                MessageRole::Assistant => {
                    let mut content_blocks = Vec::new();
                    if is_kimi_endpoint
                        && msg
                            .tool_calls
                            .as_ref()
                            .is_some_and(|calls| !calls.is_empty())
                        && msg.reasoning_content.is_some()
                    {
                        let thinking = msg.reasoning_content.as_deref().unwrap_or("");
                        // Kimi /coding expects assistant tool-call replay messages
                        // to include reasoning_content semantics; preserve it as
                        // a thinking block before any text/tool_use blocks.
                        content_blocks
                            .push(serde_json::json!({"type": "thinking", "thinking": thinking}));
                    }
                    if let Some(ref text) = msg.content {
                        if !text.is_empty() {
                            content_blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                    // Convert tool_calls to Anthropic tool_use blocks
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            let input: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(serde_json::json!({}));
                            content_blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                    }
                    if !content_blocks.is_empty() {
                        anthropic_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content_blocks,
                        }));
                    }
                }
                MessageRole::Tool => {
                    // Anthropic: tool results go as user messages with tool_result content blocks
                    let content_blocks = vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "content": msg.content.as_deref().unwrap_or(""),
                    })];
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content_blocks,
                    }));
                }
            }
        }

        (system_text, anthropic_messages)
    }

    /// Convert tool schemas to Anthropic tool format.
    fn convert_tools(tools: &[ToolSchema]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// Parse an Anthropic Messages API response into LlmResponse.
    fn parse_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut content_text = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(content_arr) = json.get("content").and_then(|c| c.as_array()) {
            for block in content_arr {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !content_text.is_empty() {
                                content_text.push('\n');
                            }
                            content_text.push_str(text);
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                            extra_content: None,
                        });
                    }
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !reasoning_content.is_empty() {
                                reasoning_content.push('\n');
                            }
                            reasoning_content.push_str(thinking);
                        }
                    }
                    _ => {}
                }
            }
        }

        let usage = json.get("usage").and_then(|u| {
            let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            Some(UsageStats {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
                estimated_cost: None,
            })
        });

        let stop_reason = json
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .map(|s| match s {
                "end_turn" => "stop".to_string(),
                "tool_use" => "tool_calls".to_string(),
                other => other.to_string(),
            });

        let model = json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        let message = Message {
            role: MessageRole::Assistant,
            content: Some(content_text),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning_content.is_empty() {
                None
            } else {
                Some(reasoning_content)
            },
            cache_control: None,
        };

        Ok(LlmResponse {
            message,
            usage,
            model,
            finish_reason: stop_reason,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
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

        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();
        let (system_text, anthropic_messages) =
            Self::convert_messages(messages, Some(self.base_url.as_str()));
        let resolved_max_tokens = Self::resolve_messages_max_tokens(max_tokens, effective_model);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": anthropic_messages,
            "max_tokens": resolved_max_tokens,
        });

        if let Some(ref sys) = system_text {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(Self::convert_tools(tools));
        }
        if let Some(eb) = extra_body {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        self.update_rate_limit(resp.headers());

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

        Self::parse_response(&resp_json)
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

            let effective_model = model.as_deref().unwrap_or(&provider.model);
            let api_key = provider.effective_api_key();
            let (system_text, anthropic_messages) = AnthropicProvider::convert_messages(
                &messages,
                Some(provider.base_url.as_str()),
            );
            let resolved_max_tokens =
                AnthropicProvider::resolve_messages_max_tokens(max_tokens, effective_model);

            let mut body = serde_json::json!({
                "model": effective_model,
                "messages": anthropic_messages,
                "max_tokens": resolved_max_tokens,
                "stream": true,
            });

            if let Some(ref sys) = system_text {
                body["system"] = serde_json::json!(sys);
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(AnthropicProvider::convert_tools(&tools));
            }
            if let Some(ref eb) = extra_body {
                if let Value::Object(map) = eb {
                    for (k, v) in map {
                        body[k] = v.clone();
                    }
                }
            }

            let url = format!("{}/v1/messages", provider.base_url.trim_end_matches('/'));

            let resp = match provider.send_with_dead_connection_recovery(&url, &api_key, &body).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            provider.update_rate_limit(resp.headers());

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
                yield Err(AgentError::LlmApi(format!("API error {status}: {body_text}")));
                return;
            }

            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();
            // Track current tool_use block index for delta accumulation
            let mut current_tool_index: u32 = 0;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk_bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AgentError::LlmApi(format!("Stream read error: {e}")));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

                while let Some(event_end) = buffer.find("\n\n") {
                    let event_block = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    let mut event_type = String::new();
                    let mut event_data = String::new();

                    for line in event_block.lines() {
                        let line = line.trim();
                        if let Some(et) = line.strip_prefix("event: ") {
                            event_type = et.trim().to_string();
                        } else if let Some(d) = line.strip_prefix("data: ") {
                            event_data = d.trim().to_string();
                        }
                    }

                    if event_data.is_empty() {
                        continue;
                    }

                    let json: Value = match serde_json::from_str(&event_data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    match event_type.as_str() {
                        "content_block_start" => {
                            let block = json.get("content_block").unwrap_or(&json);
                            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if block_type == "tool_use" {
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                let idx = json.get("index").and_then(|i| i.as_u64()).unwrap_or(current_tool_index as u64) as u32;
                                current_tool_index = idx;
                                yield Ok(StreamChunk {
                                    delta: Some(StreamDelta {
                                        content: None,
                                        tool_calls: Some(vec![ToolCallDelta {
                                            index: idx,
                                            id: Some(id),
                                            function: Some(FunctionCallDelta {
                                                name: Some(name),
                                                arguments: None,
                                            }),
                                        }]),
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                            }
                        }
                        "content_block_delta" => {
                            let delta = json.get("delta").unwrap_or(&json);
                            let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match delta_type {
                                "text_delta" => {
                                    let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: Some(text),
                                            tool_calls: None,
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                "input_json_delta" => {
                                    let partial = delta.get("partial_json").and_then(|p| p.as_str()).unwrap_or("").to_string();
                                    yield Ok(StreamChunk {
                                        delta: Some(StreamDelta {
                                            content: None,
                                            tool_calls: Some(vec![ToolCallDelta {
                                                index: current_tool_index,
                                                id: None,
                                                function: Some(FunctionCallDelta {
                                                    name: None,
                                                    arguments: Some(partial),
                                                }),
                                            }]),
                                            extra: None,
                                        }),
                                        finish_reason: None,
                                        usage: None,
                                    });
                                }
                                "thinking_delta" => {
                                    if let Some(thinking) =
                                        delta.get("thinking").and_then(|t| t.as_str())
                                    {
                                        yield Ok(StreamChunk {
                                            delta: Some(StreamDelta {
                                                content: None,
                                                tool_calls: None,
                                                extra: Some(
                                                    serde_json::json!({"thinking": thinking}),
                                                ),
                                            }),
                                            finish_reason: None,
                                            usage: None,
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        "message_delta" => {
                            let stop_reason = json
                                .get("delta")
                                .and_then(|d| d.get("stop_reason"))
                                .and_then(|s| s.as_str())
                                .map(|s| match s {
                                    "end_turn" => "stop".to_string(),
                                    "tool_use" => "tool_calls".to_string(),
                                    other => other.to_string(),
                                });
                            let usage = json.get("usage").and_then(|u| {
                                let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                Some(UsageStats {
                                    prompt_tokens: 0,
                                    completion_tokens: output,
                                    total_tokens: output,
                                    estimated_cost: None,
                                })
                            });
                            yield Ok(StreamChunk {
                                delta: None,
                                finish_reason: stop_reason,
                                usage,
                            });
                        }
                        "message_start" => {
                            // Extract usage from the initial message
                            let usage = json.get("message").and_then(|m| m.get("usage")).and_then(|u| {
                                let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                Some(UsageStats {
                                    prompt_tokens: input,
                                    completion_tokens: 0,
                                    total_tokens: input,
                                    estimated_cost: None,
                                })
                            });
                            if let Some(u) = usage {
                                yield Ok(StreamChunk {
                                    delta: None,
                                    finish_reason: None,
                                    usage: Some(u),
                                });
                            }
                        }
                        "message_stop" => {
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        .boxed()
    }
}

// ---------------------------------------------------------------------------
// OpenRouterProvider
// ---------------------------------------------------------------------------

/// OpenRouter API provider with support for OpenRouter-specific parameters.
///
/// Adds:
/// - `HTTP-Referer` and `X-Title` headers (required by OpenRouter)
/// - Support for `transforms`, `provider` preferences, `route` in extra_body
/// - Parsing of `reasoning_details` array from responses
/// - `reasoning_content` extraction
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    inner: GenericProvider,
    /// HTTP-Referer header value (required by OpenRouter).
    pub http_referer: Option<String>,
    /// X-Title header value (required by OpenRouter).
    pub x_title: Option<String>,
}

impl OpenRouterProvider {
    /// Create a new OpenRouter provider with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://openrouter.ai/api/v1", api_key, "openai/gpt-4o"),
            http_referer: None,
            x_title: None,
        }
    }

    /// Set the default model.
    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
            ..self
        }
    }

    /// Set the HTTP-Referer header (required by OpenRouter).
    pub fn with_http_referer(mut self, referer: impl Into<String>) -> Self {
        self.http_referer = Some(referer.into());
        self
    }

    /// Set the X-Title header (required by OpenRouter).
    pub fn with_x_title(mut self, title: impl Into<String>) -> Self {
        self.x_title = Some(title.into());
        self
    }

    /// Attach a credential pool for API key rotation.
    pub fn with_credential_pool(self, pool: Arc<CredentialPool>) -> Self {
        Self {
            inner: self.inner.with_credential_pool(pool),
            ..self
        }
    }

    /// Build the extra headers including OpenRouter-specific ones.
    fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = self.inner.extra_headers.clone();
        if let Some(ref referer) = self.http_referer {
            headers.push(("HTTP-Referer".to_string(), referer.clone()));
        }
        if let Some(ref title) = self.x_title {
            headers.push(("X-Title".to_string(), title.clone()));
        }
        headers
    }

    /// Merge OpenRouter-specific parameters into extra_body.
    fn merge_extra_body(extra_body: Option<&Value>) -> Option<Value> {
        // Pass through extra_body as-is; OpenRouter-specific fields like
        // `transforms`, `provider`, `route` are already valid top-level keys
        extra_body.cloned()
    }

    /// Parse an OpenRouter response, extracting reasoning_details if present.
    fn parse_openrouter_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut response = parse_openai_response(json)?;

        // Extract reasoning_content from various locations
        if let Some(reasoning) = crate::reasoning::parse_reasoning(json) {
            response.message.reasoning_content = Some(reasoning);
        }

        Ok(response)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        // Build a provider clone with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();

        let merged_extra = Self::merge_extra_body(extra_body);

        // Use GenericProvider for the actual request
        let effective_model = model.unwrap_or(&self.inner.model);
        provider.check_rate_limit().await;

        let api_key = provider.effective_api_key();
        let strict_tool_sanitize =
            GenericProvider::should_sanitize_tool_calls(merged_extra.as_ref());
        let api_messages =
            GenericProvider::sanitize_messages_for_strict_api(messages, strict_tool_sanitize);

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
        });

        if let Some(mt) = max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !tools.is_empty() {
            body["tools"] = GenericProvider::format_tools_for_openai_api(tools);
        }
        if let Some(ref eb) = merged_extra {
            if let Value::Object(map) = eb {
                for (k, v) in map {
                    body[k] = v.clone();
                }
            }
        }

        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );

        let resp = provider
            .send_with_dead_connection_recovery(&url, &api_key, &body)
            .await?;

        provider.update_rate_limit(resp.headers());

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

        Self::parse_openrouter_response(&resp_json)
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
        // Use GenericProvider's streaming with OpenRouter headers
        let mut provider = self.inner.clone();
        provider.extra_headers = self.build_headers();
        let merged_extra = Self::merge_extra_body(extra_body);

        provider.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            merged_extra.as_ref(),
        )
    }
}

// ---------------------------------------------------------------------------
// SSE chunk parsing helpers
// ---------------------------------------------------------------------------

/// Parse a single SSE data JSON object into a StreamChunk (OpenAI format).
fn parse_sse_chunk(json: &Value) -> Option<StreamChunk> {
    let choices = json.get("choices").and_then(|c| c.as_array())?;
    let choice = choices.first()?;

    let delta_obj = choice.get("delta")?;

    let content = delta_obj
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());

    let tool_calls = delta_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let index = tc.get("index").and_then(|i| i.as_u64())? as u32;
                    let id = tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
                    let function = tc.get("function").map(|f| FunctionCallDelta {
                        name: f
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        arguments: f
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string()),
                    });
                    Some(ToolCallDelta {
                        index,
                        id,
                        function,
                    })
                })
                .collect::<Vec<_>>()
        });

    let delta = if content.is_some() || tool_calls.is_some() {
        Some(StreamDelta {
            content,
            tool_calls,
            extra: None,
        })
    } else {
        None
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    // Usage may appear in the final chunk
    let usage = json.get("usage").and_then(|u| {
        Some(UsageStats {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            estimated_cost: None,
        })
    });

    Some(StreamChunk {
        delta,
        finish_reason,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Parse an OpenAI-style chat completion response.
fn parse_openai_response(json: &Value) -> Result<LlmResponse, AgentError> {
    let choices = json
        .get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| AgentError::LlmApi("No choices in response".to_string()))?;

    let choice = choices
        .first()
        .ok_or_else(|| AgentError::LlmApi("Empty choices array".to_string()))?;

    let message_obj = choice
        .get("message")
        .ok_or_else(|| AgentError::LlmApi("No message in choice".to_string()))?;

    // Parse content
    let content = message_obj
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // Parse tool calls
    let tool_calls = message_obj
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let function = tc.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let arguments = function
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}")
                        .to_string();
                    let extra_content = tc.get("extra_content").filter(|v| !v.is_null()).cloned();

                    Some(hermes_core::ToolCall {
                        id,
                        function: hermes_core::FunctionCall { name, arguments },
                        extra_content,
                    })
                })
                .collect::<Vec<_>>()
        });

    // Parse usage
    let usage = json.get("usage").and_then(|u| {
        Some(UsageStats {
            prompt_tokens: u.get("prompt_tokens")?.as_u64()? as u64,
            completion_tokens: u.get("completion_tokens")?.as_u64()? as u64,
            total_tokens: u.get("total_tokens")?.as_u64()? as u64,
            estimated_cost: None,
        })
    });

    let role = message_obj
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("assistant");

    // Extract reasoning content
    let reasoning_content = message_obj
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let message = Message {
        role: match role {
            "user" => MessageRole::User,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => MessageRole::Assistant,
        },
        content: Some(content),
        tool_calls,
        tool_call_id: None,
        name: None,
        reasoning_content,
        cache_control: None,
    };

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());

    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(LlmResponse {
        message,
        usage,
        model,
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tools_for_openai_api_shape() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read file content",
            hermes_core::JsonSchema::new("object"),
        )];
        let formatted = GenericProvider::format_tools_for_openai_api(&tools);
        let rows = formatted.as_array().expect("tools array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["type"], "function");
        assert_eq!(rows[0]["function"]["name"], "read_file");
        assert_eq!(rows[0]["function"]["description"], "Read file content");
        assert_eq!(rows[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn test_parse_openai_response_basic() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp = parse_openai_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn test_parse_openai_response_with_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gpt-4o"
        });
        let resp = parse_openai_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_parse_openai_response_with_tool_call_extra_content() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        },
                        "extra_content": {
                            "google": {
                                "thought_signature": "SIG_ABC123"
                            }
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "gemini-2.5-pro"
        });
        let resp = parse_openai_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "read_file");
        assert_eq!(
            tc[0].extra_content,
            Some(serde_json::json!({
                "google": {
                    "thought_signature": "SIG_ABC123"
                }
            }))
        );
    }

    #[test]
    fn test_parse_sse_chunk_content() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "Hello"
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(
            chunk.delta.as_ref().unwrap().content.as_deref(),
            Some("Hello")
        );
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn test_parse_sse_chunk_tool_call() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": {
                            "name": "search",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        let tc = chunk.delta.as_ref().unwrap().tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].index, 0);
        assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
    }

    #[test]
    fn test_parse_sse_chunk_finish() {
        let json = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let chunk = parse_sse_chunk(&json).unwrap();
        assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 150);
    }

    #[test]
    fn test_anthropic_convert_messages() {
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
        assert_eq!(system.as_deref(), Some("You are helpful"));
        assert_eq!(msgs.len(), 2); // user + assistant, system extracted
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn test_anthropic_convert_messages_with_tool_result() {
        let messages = vec![
            Message::system("System"),
            Message::user("Do something"),
            Message {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "tc_1".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"test.txt"}"#.to_string(),
                    },
                    extra_content: None,
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
            Message::tool_result("tc_1", "file contents here"),
        ];
        let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
        assert_eq!(system.as_deref(), Some("System"));
        assert_eq!(msgs.len(), 3); // user, assistant with tool_use, user with tool_result
                                   // Assistant message should have tool_use block
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content[0]["type"], "tool_use");
        assert_eq!(assistant_content[0]["name"], "read_file");
        // Tool result should be a user message with tool_result block
        let tool_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(tool_content[0]["type"], "tool_result");
        assert_eq!(tool_content[0]["tool_use_id"], "tc_1");
    }

    #[test]
    fn test_anthropic_convert_messages_kimi_tool_replay_preserves_reasoning_content() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_kimi".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"date"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some("provider scratchpad".to_string()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.kimi.com/coding/v1"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "provider scratchpad");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn test_anthropic_convert_messages_kimi_accepts_empty_reasoning_content() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_empty".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some(String::new()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.moonshot.ai/v1"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "");
    }

    #[test]
    fn test_anthropic_convert_messages_non_kimi_skips_thinking_block() {
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_other".to_string(),
                function: FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: Some("scratchpad".to_string()),
            cache_control: None,
        }];
        let (_, msgs) =
            AnthropicProvider::convert_messages(&messages, Some("https://api.anthropic.com"));
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
    }

    #[test]
    fn test_anthropic_parse_response() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Here is the answer."}
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Here is the answer."));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 100);
        assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 50);
    }

    #[test]
    fn test_anthropic_parse_response_preserves_thinking_as_reasoning_content() {
        let json = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "step 1"},
                {"type": "text", "text": "answer"}
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("answer"));
        assert_eq!(resp.message.reasoning_content.as_deref(), Some("step 1"));
    }

    #[test]
    fn test_anthropic_parse_response_with_tool_use() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "read_file",
                    "input": {"path": "test.txt"}
                }
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 80
            }
        });
        let resp = AnthropicProvider::parse_response(&json).unwrap();
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "toolu_123");
        assert_eq!(tc[0].function.name, "read_file");
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42.",
                    "reasoning_content": "Let me think step by step..."
                },
                "finish_reason": "stop"
            }],
            "model": "deepseek/deepseek-r1",
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 30,
                "total_tokens": 80
            }
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("The answer is 42."));
        assert_eq!(
            resp.message.reasoning_content.as_deref(),
            Some("Let me think step by step...")
        );
    }

    #[test]
    fn test_openrouter_parse_response_with_reasoning_details() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Final answer.",
                    "reasoning_details": [
                        {"type": "text", "text": "Step 1"},
                        {"type": "text", "text": "Step 2"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "model": "openai/o1-preview"
        });
        let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
        let reasoning = resp.message.reasoning_content.as_deref().unwrap();
        assert!(reasoning.contains("Step 1"));
        assert!(reasoning.contains("Step 2"));
    }

    #[test]
    fn test_openrouter_build_headers() {
        let provider = OpenRouterProvider::new("key")
            .with_http_referer("https://example.com")
            .with_x_title("My App");
        let headers = provider.build_headers();
        assert!(headers
            .iter()
            .any(|(k, v)| k == "HTTP-Referer" && v == "https://example.com"));
        assert!(headers.iter().any(|(k, v)| k == "X-Title" && v == "My App"));
    }

    #[test]
    fn test_anthropic_convert_tools() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read a file",
            hermes_core::JsonSchema::new("object"),
        )];
        let converted = AnthropicProvider::convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "read_file");
        assert_eq!(converted[0]["description"], "Read a file");
        assert!(converted[0].get("input_schema").is_some());
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_prefers_positive_request() {
        let resolved =
            AnthropicProvider::resolve_messages_max_tokens(Some(8192), "claude-opus-4-1");
        assert_eq!(resolved, 8192);
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_zero_falls_back_to_model_default() {
        let resolved = AnthropicProvider::resolve_messages_max_tokens(Some(0), "claude-opus-4-6");
        assert!(resolved > 0);
        assert_eq!(resolved, get_anthropic_max_output("claude-opus-4-6"));
    }

    #[test]
    fn test_anthropic_resolve_messages_max_tokens_none_falls_back_to_model_default() {
        let resolved = AnthropicProvider::resolve_messages_max_tokens(None, "claude-sonnet-4-6");
        assert!(resolved > 0);
        assert_eq!(resolved, get_anthropic_max_output("claude-sonnet-4-6"));
    }
}
