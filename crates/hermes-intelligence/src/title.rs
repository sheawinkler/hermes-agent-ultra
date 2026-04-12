//! Title generator — generates concise conversation titles using a small/fast model.
//!
//! Requirement 16.4

use std::sync::Arc;

use hermes_core::{AgentError, LlmProvider, Message, ToolSchema};

// ---------------------------------------------------------------------------
// TitleError
// ---------------------------------------------------------------------------

/// Errors that can occur during title generation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TitleError {
    #[error("LLM provider error: {0}")]
    LlmError(String),

    #[error("No messages provided for title generation")]
    NoMessages,

    #[error("Title generation produced an empty result")]
    EmptyResult,
}

impl From<AgentError> for TitleError {
    fn from(err: AgentError) -> Self {
        TitleError::LlmError(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// TitleGenerator
// ---------------------------------------------------------------------------

/// Generates concise conversation titles from message history.
pub struct TitleGenerator {
    client: reqwest::Client,
    llm_provider: Arc<dyn LlmProvider>,
    /// The model name to use for title generation (should be small/fast).
    model: String,
    /// Maximum number of messages to include before truncation.
    max_messages: usize,
    /// Maximum characters per message before truncation.
    max_message_chars: usize,
}

impl TitleGenerator {
    /// Create a new title generator.
    ///
    /// Uses the given LLM provider with the specified model (preferably
    /// a small, fast model like `gpt-4o-mini` or `claude-haiku`).
    pub fn new(llm_provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            llm_provider,
            model: model.into(),
            max_messages: 10,
            max_message_chars: 200,
        }
    }

    /// Set the maximum number of messages to include for title generation.
    pub fn with_max_messages(mut self, max: usize) -> Self {
        self.max_messages = max;
        self
    }

    /// Set the maximum characters per message before truncation.
    pub fn with_max_message_chars(mut self, max: usize) -> Self {
        self.max_message_chars = max;
        self
    }

    /// Generate a concise title for the given conversation messages.
    ///
    /// Takes the first few messages, truncates long ones, and asks the LLM
    /// to produce a short descriptive title.
    pub async fn generate_title(&self, messages: &[Message]) -> Result<String, TitleError> {
        if messages.is_empty() {
            return Err(TitleError::NoMessages);
        }

        // Build a truncated summary of the conversation
        let truncated = self.truncate_messages(messages);

        let system_prompt = "You are a title generator. Given a conversation, produce a short, concise title (5-8 words). \
            Do not use quotes. Just output the title text and nothing else.";

        let mut title_messages = vec![Message::system(system_prompt)];
        title_messages.push(Message::user(&truncated));

        let response = self
            .llm_provider
            .chat_completion(
                &title_messages,
                &[],
                Some(32),  // max_tokens — short output
                Some(0.3), // low temperature for consistency
                Some(&self.model),
                None,
            )
            .await
            .map_err(TitleError::from)?;

        let title = response
            .message
            .content
            .unwrap_or_default()
            .trim()
            .to_string();

        if title.is_empty() {
            return Err(TitleError::EmptyResult);
        }

        // Strip surrounding quotes if present
        let title = title
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(&title)
            .to_string();

        Ok(title)
    }

    /// Truncate messages into a single string for the title prompt.
    fn truncate_messages(&self, messages: &[Message]) -> String {
        let mut parts = Vec::new();
        let slice = if messages.len() > self.max_messages {
            &messages[..self.max_messages]
        } else {
            messages
        };

        for msg in slice {
            if let Some(content) = &msg.content {
                let role = match msg.role {
                    hermes_core::types::MessageRole::System => "System",
                    hermes_core::types::MessageRole::User => "User",
                    hermes_core::types::MessageRole::Assistant => "Assistant",
                    hermes_core::types::MessageRole::Tool => "Tool",
                };
                let truncated_content = if content.len() > self.max_message_chars {
                    &content[..self.max_message_chars]
                } else {
                    content
                };
                parts.push(format!("{}: {}", role, truncated_content));
            }
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_messages() {
        let gen = TitleGenerator::for_test();
        let messages = vec![
            Message::user("Hello, can you help me with Rust programming?"),
            Message::assistant("Of course! I'd be happy to help with Rust."),
        ];
        let result = gen.truncate_messages(&messages);
        assert!(result.contains("User:"));
        assert!(result.contains("Assistant:"));
    }

    #[test]
    fn test_truncate_long_messages() {
        let gen = TitleGenerator::for_test().with_max_message_chars(20);
        let messages = vec![Message::user("This is a very long message that should be truncated")];
        let result = gen.truncate_messages(&messages);
        // The truncated content should be at most 20 chars
        let content_line = result.split(':').nth(1).unwrap().trim();
        assert!(content_line.len() <= 20);
    }

    #[test]
    fn test_empty_messages_error() {
        let gen = TitleGenerator::for_test();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(gen.generate_title(&[]));
        assert!(matches!(result, Err(TitleError::NoMessages)));
    }

    // Helper for tests that don't need a real provider
    impl TitleGenerator {
        #[cfg(test)]
        fn for_test() -> Self {
            use std::sync::Arc;
            // We can't create a real provider in unit tests without mocking,
            // but we need the struct for truncation tests.
            // This uses a minimal mock that will panic if called.
            struct MockProvider;
            #[async_trait::async_trait]
            impl LlmProvider for MockProvider {
                async fn chat_completion(
                    &self,
                    _messages: &[Message],
                    _tools: &[ToolSchema],
                    _max_tokens: Option<u32>,
                    _temperature: Option<f64>,
                    _model: Option<&str>,
                    _extra_body: Option<&serde_json::Value>,
                ) -> Result<hermes_core::LlmResponse, AgentError> {
                    panic!("MockProvider should not be called in this test")
                }

                fn chat_completion_stream(
                    &self,
                    _messages: &[Message],
                    _tools: &[ToolSchema],
                    _max_tokens: Option<u32>,
                    _temperature: Option<f64>,
                    _model: Option<&str>,
                    _extra_body: Option<&serde_json::Value>,
                ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
                    panic!("MockProvider should not be called in this test")
                }
            }
            Self {
                client: reqwest::Client::new(),
                llm_provider: Arc::new(MockProvider),
                model: "test-model".to_string(),
                max_messages: 10,
                max_message_chars: 200,
            }
        }
    }
}