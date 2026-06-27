//! OpenAI Responses API (Codex) protocol implementation.
//!
//! The Responses API (`POST /v1/responses`) is used by OpenAI Codex and similar
//! models. It differs from chat completions in request/response format.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::{
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
    Client,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use hermes_core::{
    AgentError, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

use crate::credential_pool::CredentialPool;
use crate::provider::{
    codex_cloudflare_headers, is_openai_dynamic_model_alias, OPENAI_CODEX_BASE_URL,
    OPENAI_CODEX_DYNAMIC_WIRE_MODEL,
};
use crate::rate_limit::RateLimitTracker;

const CODEX_RESPONSES_BETA_HEADER: &str = "responses=2026-02-06";

fn request_timeout_duration(seconds: Option<f64>) -> Option<Duration> {
    seconds.and_then(|value| {
        if value.is_finite() && value > 0.0 {
            Duration::try_from_secs_f64(value).ok()
        } else {
            None
        }
    })
}

fn build_codex_http_client(request_timeout: Option<Duration>) -> Client {
    let mut builder = Client::builder();
    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().unwrap_or_else(|err| {
        tracing::warn!("failed to build Codex HTTP client: {}", err);
        Client::new()
    })
}

/// OpenAI Responses API provider for Codex models.
#[derive(Debug, Clone)]
pub struct CodexProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub headers: Vec<(String, String)>,
    chatgpt_codex_backend: bool,
    client: Client,
    request_timeout: Option<Duration>,
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl CodexProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let request_timeout = None;
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: api_key.into(),
            model: "codex-mini-latest".to_string(),
            headers: Vec::new(),
            chatgpt_codex_backend: false,
            client: build_codex_http_client(request_timeout),
            request_timeout,
            rate_limiter: None,
            credential_pool: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    pub fn with_optional_request_timeout_seconds(mut self, seconds: Option<f64>) -> Self {
        self.request_timeout = request_timeout_duration(seconds);
        self.client = build_codex_http_client(self.request_timeout);
        self
    }

    pub fn with_request_timeout_seconds(self, seconds: f64) -> Self {
        self.with_optional_request_timeout_seconds(Some(seconds))
    }

    #[cfg(test)]
    pub(crate) fn configured_request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    pub fn with_rate_limiter(mut self, tracker: Arc<RateLimitTracker>) -> Self {
        self.rate_limiter = Some(tracker);
        self
    }

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

    fn uses_chatgpt_codex_backend(&self) -> bool {
        self.chatgpt_codex_backend
            || self
                .base_url
                .trim()
                .to_ascii_lowercase()
                .contains("chatgpt.com/backend-api/codex")
    }

    fn effective_wire_model(&self, requested_model: &str) -> String {
        let requested_model = requested_model.trim();
        if self.uses_chatgpt_codex_backend() && is_openai_dynamic_model_alias(requested_model) {
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL.to_string()
        } else {
            requested_model.to_string()
        }
    }

    fn request_headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = self.headers.clone();
        if self.uses_chatgpt_codex_backend() {
            for header in codex_cloudflare_headers(Some(api_key)) {
                if !headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&header.0))
                {
                    headers.push(header);
                }
            }
            if !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("OpenAI-Beta"))
            {
                headers.push((
                    "OpenAI-Beta".to_string(),
                    CODEX_RESPONSES_BETA_HEADER.to_string(),
                ));
            }
        }
        headers
    }

    fn request_builder(&self, url: &str, api_key: &str, body: &Value) -> reqwest::RequestBuilder {
        let accept = if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
            "text/event-stream"
        } else {
            "application/json"
        };
        let mut request = self
            .client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", api_key))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, accept);
        for (name, value) in self.request_headers(api_key) {
            request = request.header(name, value);
        }
        request.json(body)
    }

    pub fn openai_pro(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let mut provider = Self::new(api_key.as_str())
            .with_model(model)
            .with_base_url(OPENAI_CODEX_BASE_URL)
            .with_headers(codex_cloudflare_headers(Some(api_key.as_str())));
        provider.chatgpt_codex_backend = true;
        provider
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

    /// Convert internal non-system messages to the Responses API input format.
    ///
    /// The Responses API uses a flat `input` array with items of different types:
    /// - `{ "role": "user", "content": "..." }`
    /// - `{ "role": "assistant", "content": "..." }`
    /// - `{ "type": "function_call", "name": "...", "arguments": "...", "call_id": "..." }`
    /// - `{ "type": "function_call_output", "call_id": "...", "output": "..." }`
    fn convert_input(messages: &[Message]) -> Vec<Value> {
        let mut input = Vec::new();
        for msg in messages {
            match msg.role {
                MessageRole::System => {}
                MessageRole::User => {
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": msg.content.as_deref().unwrap_or("")
                        }]
                    }));
                }
                MessageRole::Assistant => {
                    if let Some(ref text) = msg.content {
                        if !text.is_empty() {
                            input.push(serde_json::json!({
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": text
                                }]
                            }));
                        }
                    }
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                                "call_id": tc.id
                            }));
                        }
                    }
                }
                MessageRole::Tool => {
                    input.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "output": msg.content.as_deref().unwrap_or("")
                    }));
                }
            }
        }
        input
    }

    fn convert_instructions(messages: &[Message]) -> Option<String> {
        let instructions = messages
            .iter()
            .filter(|msg| msg.role == MessageRole::System)
            .filter_map(|msg| msg.content.as_deref())
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        }
    }

    fn default_instructions() -> &'static str {
        "You are Hermes Agent Ultra. Follow the user's instructions exactly."
    }

    fn should_forward_extra_body_key(key: &str) -> bool {
        !matches!(key, "strict_api" | "strict_tool_calls" | "provider_strict")
    }

    /// Convert tool schemas to Responses API function format.
    fn convert_tools(tools: &[ToolSchema]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            })
            .collect()
    }

    fn build_body(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        effective_model: &str,
        extra_body: Option<&Value>,
        stream: bool,
    ) -> Value {
        let mut body = serde_json::json!({
            "model": effective_model,
            "instructions": Self::convert_instructions(messages)
                .unwrap_or_else(|| Self::default_instructions().to_string()),
            "input": Self::convert_input(messages),
        });

        if stream {
            body["stream"] = serde_json::json!(true);
        }
        if let Some(mt) = max_tokens {
            body["max_output_tokens"] = serde_json::json!(mt);
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
                    if !Self::should_forward_extra_body_key(k) {
                        continue;
                    }
                    body[k] = v.clone();
                }
            }
        }
        if self.uses_chatgpt_codex_backend() {
            body["stream"] = serde_json::json!(true);
            body["store"] = serde_json::json!(false);
        }
        if let Some(model) = body.get("model").and_then(Value::as_str) {
            body["model"] = serde_json::json!(self.effective_wire_model(model));
        }
        body
    }

    /// Parse a Responses API response.
    fn parse_response(json: &Value) -> Result<LlmResponse, AgentError> {
        let mut content_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
            for item in output {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match item_type {
                    "message" => {
                        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                            for block in content {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    content_text.push_str(text);
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let id = item
                            .get("call_id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        let extra_content =
                            item.get("extra_content").filter(|v| !v.is_null()).cloned();
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                            extra_content,
                        });
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

        let model = json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        let stop_reason = json
            .get("status")
            .and_then(|s| s.as_str())
            .map(|s| match s {
                "completed" => "stop".to_string(),
                "incomplete" => "length".to_string(),
                other => other.to_string(),
            });

        let message = Message {
            role: MessageRole::Assistant,
            content: if content_text.is_empty() {
                None
            } else {
                Some(content_text)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            anthropic_content_blocks: None,
            cache_control: None,
        };

        Ok(LlmResponse {
            message,
            usage,
            model,
            finish_reason: stop_reason,
        })
    }

    fn parse_sse_event_block(event_block: &str) -> Option<(String, Value)> {
        let mut event_type = String::new();
        let mut event_data = String::new();

        for line in event_block.lines() {
            let line = line.trim();
            if let Some(et) = line.strip_prefix("event: ") {
                event_type = et.trim().to_string();
            } else if let Some(d) = line.strip_prefix("data: ") {
                event_data.push_str(d.trim());
            }
        }

        if event_data.is_empty() {
            return None;
        }

        let json: Value = serde_json::from_str(&event_data).ok()?;
        Some((event_type, json))
    }

    async fn collect_streaming_response(
        &self,
        resp: reqwest::Response,
        effective_model: &str,
    ) -> Result<LlmResponse, AgentError> {
        let mut byte_stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut content_text = String::new();
        let mut completed_response: Option<Value> = None;

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk_bytes =
                chunk_result.map_err(|e| AgentError::LlmApi(format!("Stream read error: {e}")))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

            while let Some(event_end) = buffer.find("\n\n") {
                let event_block = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();
                let Some((event_type, json)) = Self::parse_sse_event_block(&event_block) else {
                    continue;
                };

                match event_type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                            content_text.push_str(delta);
                        }
                    }
                    "response.completed" => {
                        completed_response = json.get("response").cloned();
                    }
                    "response.failed" => {
                        return Err(AgentError::LlmApi(format!("response failed: {json}")));
                    }
                    _ => {}
                }
            }
        }

        if let Some(response) = completed_response {
            if let Ok(parsed) = Self::parse_response(&response) {
                if parsed.message.content.is_some() || parsed.message.tool_calls.is_some() {
                    return Ok(parsed);
                }
            }
        }

        Ok(LlmResponse {
            message: Message {
                role: MessageRole::Assistant,
                content: if content_text.is_empty() {
                    None
                } else {
                    Some(content_text)
                },
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
            usage: None,
            model: effective_model.to_string(),
            finish_reason: Some("stop".to_string()),
        })
    }
}

#[async_trait]
impl LlmProvider for CodexProvider {
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
        let effective_model = self.effective_wire_model(model.unwrap_or(&self.model));
        let api_key = self.effective_api_key();

        let body = self.build_body(
            messages,
            tools,
            max_tokens,
            temperature,
            effective_model.as_str(),
            extra_body,
            false,
        );

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let resp = self
            .request_builder(&url, &api_key, &body)
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("HTTP request failed: {e}")))?;

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

        if self.uses_chatgpt_codex_backend() {
            return self
                .collect_streaming_response(resp, effective_model.as_str())
                .await;
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
            let effective_model = provider.effective_wire_model(model.as_deref().unwrap_or(&provider.model));
            let api_key = provider.effective_api_key();

            let body = provider.build_body(
                &messages,
                &tools,
                max_tokens,
                temperature,
                effective_model.as_str(),
                extra_body.as_ref(),
                true,
            );

            let url = format!("{}/responses", provider.base_url.trim_end_matches('/'));

            let resp = match provider
                .request_builder(&url, &api_key, &body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(AgentError::LlmApi(format!("HTTP request failed: {e}")));
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

                    let Some((event_type, json)) = CodexProvider::parse_sse_event_block(&event_block) else {
                        continue;
                    };

                    match event_type.as_str() {
                        "response.output_text.delta" => {
                            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                                yield Ok(StreamChunk {
                                    delta: Some(StreamDelta {
                                        content: Some(delta.to_string()),
                                        tool_calls: None,
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            let call_id = json.get("call_id").and_then(|i| i.as_str()).map(|s| s.to_string());
                            let name = json.get("name").and_then(|n| n.as_str()).map(|s| s.to_string());
                            let args_delta = json.get("delta").and_then(|d| d.as_str()).map(|s| s.to_string());
                            yield Ok(StreamChunk {
                                delta: Some(StreamDelta {
                                    content: None,
                                    tool_calls: Some(vec![ToolCallDelta {
                                        index: 0,
                                        id: call_id,
                                        function: Some(FunctionCallDelta {
                                            name,
                                            arguments: args_delta,
                                        }),
                                    }]),
                                    extra: None,
                                }),
                                finish_reason: None,
                                usage: None,
                            });
                        }
                        "response.completed" => {
                            let usage = json.get("response").and_then(|r| r.get("usage")).and_then(|u| {
                                let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                Some(UsageStats {
                                    prompt_tokens: input,
                                    completion_tokens: output,
                                    total_tokens: input + output,
                                    estimated_cost: None,
                                })
                            });
                            yield Ok(StreamChunk {
                                delta: None,
                                finish_reason: Some("stop".to_string()),
                                usage,
                            });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_provider_request_timeout_seconds_configures_client() {
        let provider = CodexProvider::new("sk-test").with_request_timeout_seconds(45.0);

        assert_eq!(
            provider.configured_request_timeout(),
            Some(Duration::from_secs(45))
        );
    }

    #[test]
    fn codex_provider_ignores_invalid_request_timeout_seconds() {
        for value in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::INFINITY),
            Some(f64::NAN),
        ] {
            let provider =
                CodexProvider::new("sk-test").with_optional_request_timeout_seconds(value);
            assert_eq!(provider.configured_request_timeout(), None);
        }
    }

    #[test]
    fn test_convert_input_basic() {
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi!"),
        ];
        let input = CodexProvider::convert_input(&messages);
        assert_eq!(
            CodexProvider::convert_instructions(&messages).as_deref(),
            Some("You are helpful")
        );
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["content"][0]["text"], "Hi!");
    }

    #[test]
    fn test_convert_input_with_tool_calls() {
        let messages = vec![
            Message::user("Read file"),
            Message {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"test.txt"}"#.to_string(),
                    },
                    extra_content: None,
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
            Message::tool_result("call_1", "file contents"),
        ];
        let input = CodexProvider::convert_input(&messages);
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "read_file");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_1");
    }

    #[test]
    fn openai_pro_body_uses_chatgpt_codex_contract() {
        let provider = CodexProvider::openai_pro("token", "gpt-5.5");
        let messages = vec![Message::system("Be exact"), Message::user("Say ok")];
        let extra_body = serde_json::json!({
            "strict_api": true,
            "strict_tool_calls": true,
            "provider_strict": true,
            "service_tier": "fast"
        });
        let body = provider.build_body(
            &messages,
            &[],
            None,
            None,
            "gpt-5.5",
            Some(&extra_body),
            false,
        );

        assert_eq!(body["model"], "gpt-5.5");
        assert_eq!(body["instructions"], "Be exact");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["service_tier"], "fast");
        assert!(body.get("strict_api").is_none());
        assert!(body.get("strict_tool_calls").is_none());
        assert!(body.get("provider_strict").is_none());
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn openai_pro_dynamic_alias_uses_supported_chatgpt_wire_model() {
        let provider = CodexProvider::openai_pro("token", "dynamic");
        assert_eq!(
            provider.effective_wire_model("dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            provider.effective_wire_model("openai:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            provider.effective_wire_model("openai-codex:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(provider.effective_wire_model("gpt-5.5"), "gpt-5.5");
        assert_eq!(
            provider.effective_wire_model("dynamic-runtime-model"),
            "dynamic-runtime-model"
        );

        let body = provider.build_body(
            &[Message::user("Say ok")],
            &[],
            None,
            None,
            provider.effective_wire_model("dynamic").as_str(),
            None,
            false,
        );

        assert_eq!(body["model"], OPENAI_CODEX_DYNAMIC_WIRE_MODEL);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn openai_pro_extra_body_cannot_restore_dynamic_wire_model() {
        let provider = CodexProvider::openai_pro("token", "dynamic");
        let extra_body = serde_json::json!({
            "model": "dynamic",
            "service_tier": "priority"
        });

        let body = provider.build_body(
            &[Message::user("Say ok")],
            &[],
            None,
            None,
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL,
            Some(&extra_body),
            false,
        );

        assert_eq!(body["model"], OPENAI_CODEX_DYNAMIC_WIRE_MODEL);
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn codex_api_dynamic_alias_stays_literal_outside_chatgpt_backend() {
        let provider = CodexProvider::new("sk-test").with_model("dynamic");

        assert_eq!(provider.effective_wire_model("dynamic"), "dynamic");
    }

    #[test]
    fn openai_pro_request_uses_json_body_and_sse_accept_contract() {
        let token = "token";
        let provider = CodexProvider::openai_pro(token, "gpt-5.5");
        let body = provider.build_body(
            &[Message::user("Say ok")],
            &[],
            None,
            None,
            "gpt-5.5",
            None,
            false,
        );
        let request = provider
            .request_builder(
                &format!("{}/responses", provider.base_url.trim_end_matches('/')),
                token,
                &body,
            )
            .build()
            .expect("request");
        let headers = request.headers();

        assert_eq!(
            headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers.get(ACCEPT).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer token")
        );
        assert_eq!(
            headers.get("OpenAI-Beta").and_then(|v| v.to_str().ok()),
            Some(CODEX_RESPONSES_BETA_HEADER)
        );
        assert_eq!(
            headers.get("originator").and_then(|v| v.to_str().ok()),
            Some("codex_cli_rs")
        );
    }

    #[test]
    fn test_parse_response_text() {
        let json = serde_json::json!({
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Hello!"}]
                }
            ],
            "model": "codex-mini-latest",
            "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let resp = CodexProvider::parse_response(&json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_parse_response_tool_call() {
        let json = serde_json::json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "fc_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"test.txt\"}"
                }
            ],
            "model": "codex-mini-latest",
            "status": "completed"
        });
        let resp = CodexProvider::parse_response(&json).unwrap();
        let tc = resp.message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "fc_1");
        assert_eq!(tc[0].function.name, "read_file");
    }
}
