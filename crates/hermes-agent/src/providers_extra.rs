//! Additional LLM provider implementations for non-standard APIs.
//!
//! - [`QwenProvider`]: Alibaba Tongyi Qianwen (通义千问)
//! - [`KimiProvider`]: Moonshot AI (月之暗面)
//! - [`MiniMaxProvider`]: MiniMax
//! - [`NousProvider`]: Nous Research
//! - [`CopilotProvider`]: GitHub Copilot ACP

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};

use crate::provider::GenericProvider;

fn openrouter_compatible_extra_body(extra_body: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(map)) = extra_body else {
        return extra_body.cloned();
    };

    let mut cleaned = map.clone();
    cleaned.remove("strict_tool_calls");
    cleaned.remove("strict_api");
    cleaned.remove("provider_strict");

    // Nous' inference API is OpenRouter-compatible. Use the documented
    // cross-provider reasoning object instead of leaking provider-specific or
    // Hermes-local control keys into the request body.
    if !cleaned.contains_key("reasoning") {
        if let Some(effort) = cleaned.remove("reasoning_effort") {
            cleaned.insert(
                "reasoning".to_string(),
                serde_json::json!({ "effort": effort }),
            );
        }
    } else {
        cleaned.remove("reasoning_effort");
    }

    Some(Value::Object(cleaned))
}

// ---------------------------------------------------------------------------
// QwenProvider — Alibaba Tongyi Qianwen (通义千问)
// ---------------------------------------------------------------------------

/// Alibaba Tongyi Qianwen provider via the DashScope OpenAI-compatible endpoint.
///
/// Default base URL: `https://dashscope.aliyuncs.com/compatible-mode/v1`
/// Default model: `qwen-turbo`
#[derive(Debug, Clone)]
pub struct QwenProvider {
    inner: GenericProvider,
}

impl QwenProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                api_key,
                "qwen-turbo",
            )
            .with_provider_profile("qwen-oauth"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for QwenProvider {
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
// KimiProvider — Moonshot AI (月之暗面)
// ---------------------------------------------------------------------------

/// Moonshot AI (Kimi) provider.
///
/// Default base URL: `https://api.moonshot.cn/v1`
/// Default model: `moonshot-v1-8k`
#[derive(Debug, Clone)]
pub struct KimiProvider {
    inner: GenericProvider,
}

impl KimiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.moonshot.cn/v1", api_key, "moonshot-v1-8k")
                .with_provider_profile("kimi-coding"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for KimiProvider {
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
// MiniMaxProvider
// ---------------------------------------------------------------------------

/// MiniMax provider.
///
/// Default base URL: `https://api.minimax.chat/v1`
/// Default model: `abab6.5s-chat`
#[derive(Debug, Clone)]
pub struct MiniMaxProvider {
    inner: GenericProvider,
}

impl MiniMaxProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.minimax.chat/v1", api_key, "abab6.5s-chat"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for MiniMaxProvider {
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
// NousProvider — Nous Research
// ---------------------------------------------------------------------------

/// Nous Research inference API provider.
///
/// Default base URL: `https://inference-api.nousresearch.com/v1`
/// Default model: `hermes-3-llama-3.1-405b`
#[derive(Debug, Clone)]
pub struct NousProvider {
    inner: GenericProvider,
}

impl NousProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(
                "https://inference-api.nousresearch.com/v1",
                api_key,
                "hermes-3-llama-3.1-405b",
            )
            .with_provider_profile("nous"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for NousProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let extra_body = openrouter_compatible_extra_body(extra_body);
        self.inner
            .chat_completion(
                messages,
                tools,
                max_tokens,
                temperature,
                model,
                extra_body.as_ref(),
            )
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
        let extra_body = openrouter_compatible_extra_body(extra_body);
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body.as_ref(),
        )
    }
}

// ---------------------------------------------------------------------------
// CopilotProvider — GitHub Copilot ACP
// ---------------------------------------------------------------------------

/// GitHub Copilot ACP provider with a configurable base URL (obtained via OAuth flow).
///
/// Default model: `gpt-4o`
#[derive(Debug, Clone)]
pub struct CopilotProvider {
    inner: GenericProvider,
}

impl CopilotProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(base_url, api_key, "gpt-4o"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for CopilotProvider {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn qwen_provider_defaults() {
        let p = QwenProvider::new("test-key");
        assert_eq!(
            p.inner.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(p.inner.model, "qwen-turbo");
    }

    #[test]
    fn qwen_provider_with_model() {
        let p = QwenProvider::new("test-key").with_model("qwen-max");
        assert_eq!(p.inner.model, "qwen-max");
    }

    #[test]
    fn kimi_provider_defaults() {
        let p = KimiProvider::new("test-key");
        assert_eq!(p.inner.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(p.inner.model, "moonshot-v1-8k");
    }

    #[test]
    fn kimi_provider_with_model() {
        let p = KimiProvider::new("test-key").with_model("moonshot-v1-128k");
        assert_eq!(p.inner.model, "moonshot-v1-128k");
    }

    #[test]
    fn minimax_provider_defaults() {
        let p = MiniMaxProvider::new("test-key");
        assert_eq!(p.inner.base_url, "https://api.minimax.chat/v1");
        assert_eq!(p.inner.model, "abab6.5s-chat");
    }

    #[test]
    fn nous_provider_defaults() {
        let p = NousProvider::new("test-key");
        assert_eq!(
            p.inner.base_url,
            "https://inference-api.nousresearch.com/v1"
        );
        assert_eq!(p.inner.model, "hermes-3-llama-3.1-405b");
    }

    #[test]
    fn nous_extra_body_uses_openrouter_reasoning_shape_and_strips_local_controls() {
        let extra = serde_json::json!({
            "strict_api": true,
            "provider_strict": true,
            "reasoning_effort": "xhigh",
            "temperature": 0.1
        });
        let cleaned = openrouter_compatible_extra_body(Some(&extra)).expect("cleaned body");
        assert!(cleaned.get("strict_api").is_none());
        assert!(cleaned.get("provider_strict").is_none());
        assert!(cleaned.get("reasoning_effort").is_none());
        assert_eq!(cleaned["reasoning"]["effort"], "xhigh");
        assert_eq!(cleaned["temperature"], 0.1);
    }

    #[test]
    fn copilot_provider_custom_base_url() {
        let p = CopilotProvider::new("https://copilot.example.com/v1", "token");
        assert_eq!(p.inner.base_url, "https://copilot.example.com/v1");
        assert_eq!(p.inner.model, "gpt-4o");
    }

    #[test]
    fn copilot_provider_with_model() {
        let p = CopilotProvider::new("https://copilot.example.com/v1", "token")
            .with_model("gpt-4o-mini");
        assert_eq!(p.inner.model, "gpt-4o-mini");
    }

    #[test]
    fn openai_compatible_extra_providers_apply_request_timeout_seconds() {
        let timeout = Some(Duration::from_secs(45));

        assert_eq!(
            QwenProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            KimiProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            MiniMaxProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            NousProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            CopilotProvider::new("https://copilot.example.com/v1", "token")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
    }
}
