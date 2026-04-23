//! OpenAI Responses API (Codex) protocol implementation.
//!
//! The Responses API (`POST /v1/responses`) is used by OpenAI Codex and similar
//! models. It differs from chat completions in request/response format.

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;

use hermes_core::{
    AgentError, FunctionCall, FunctionCallDelta, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, StreamDelta, ToolCall, ToolCallDelta, ToolSchema, UsageStats,
};

use crate::credential_pool::CredentialPool;
use crate::rate_limit::RateLimitTracker;

/// OpenAI Responses API provider for Codex models.
#[derive(Debug, Clone)]
pub struct CodexProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    client: Client,
    pub rate_limiter: Option<Arc<RateLimitTracker>>,
    pub credential_pool: Option<Arc<CredentialPool>>,
}

impl CodexProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: api_key.into(),
            model: "codex-mini-latest".to_string(),
            client: Client::new(),
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

    /// Convert internal messages to the Responses API input format.
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
                MessageRole::System => {
                    input.push(serde_json::json!({
                        "role": "system",
                        "content": msg.content.as_deref().unwrap_or("")
                    }));
                }
                MessageRole::User => {
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content.as_deref().unwrap_or("")
                    }));
                }
                MessageRole::Assistant => {
                    if let Some(ref text) = msg.content {
                        if !text.is_empty() {
                            input.push(serde_json::json!({
                                "role": "assistant",
                                "content": text
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
        let effective_model = model.unwrap_or(&self.model);
        let api_key = self.effective_api_key();

        let mut body = serde_json::json!({
            "model": effective_model,
            "input": Self::convert_input(messages),
        });

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
                    body[k] = v.clone();
                }
            }
        }

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
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

            let mut body = serde_json::json!({
                "model": effective_model,
                "input": CodexProvider::convert_input(&messages),
                "stream": true,
            });

            if let Some(mt) = max_tokens {
                body["max_output_tokens"] = serde_json::json!(mt);
            }
            if let Some(temp) = temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(CodexProvider::convert_tools(&tools));
            }
            if let Some(ref eb) = extra_body {
                if let Value::Object(map) = eb {
                    for (k, v) in map {
                        body[k] = v.clone();
                    }
                }
            }

            let url = format!("{}/responses", provider.base_url.trim_end_matches('/'));

            let resp = match provider.client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
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

                    if event_data.is_empty() { continue; }

                    let json: Value = match serde_json::from_str(&event_data) {
                        Ok(v) => v,
                        Err(_) => continue,
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
    fn test_convert_input_basic() {
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi!"),
        ];
        let input = CodexProvider::convert_input(&messages);
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[2]["role"], "assistant");
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
