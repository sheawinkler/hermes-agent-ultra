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

    /// Set an optional total request timeout used by this provider and rebuilds.
    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }

    /// Add a custom header to every OpenAI-compatible request.
    pub fn with_header(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_header(key, value),
        }
    }

    /// Add several custom headers to every OpenAI-compatible request.
    pub fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (key, value) in headers {
            self = self.with_header(key, value);
        }
        self
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

