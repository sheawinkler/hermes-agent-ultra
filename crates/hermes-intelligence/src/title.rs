//! Title generator — generates concise conversation titles using a small/fast model.
//!
//! Requirement 16.4

use std::sync::Arc;

use hermes_core::{AgentError, LlmProvider, Message};

const MAX_TITLE_CHARS: usize = 80;
const TITLE_ELLIPSIS_CHARS: usize = 3;

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

        let raw_title = response
            .message
            .content
            .unwrap_or_default()
            .trim()
            .to_string();

        clean_generated_title(&raw_title).ok_or(TitleError::EmptyResult)
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

fn clean_generated_title(raw_title: &str) -> Option<String> {
    let mut title = raw_title.trim();
    if title.is_empty() {
        return None;
    }

    title = strip_matching_quotes(title).trim();
    if title
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("title:"))
    {
        title = title[6..].trim();
        title = strip_matching_quotes(title).trim();
    }

    if title.is_empty() {
        return None;
    }

    if title.chars().count() > MAX_TITLE_CHARS {
        let keep = MAX_TITLE_CHARS.saturating_sub(TITLE_ELLIPSIS_CHARS);
        let mut truncated = title.chars().take(keep).collect::<String>();
        truncated.push_str("...");
        return Some(truncated);
    }

    Some(title.to_string())
}

fn strip_matching_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{LlmResponse, MessageRole, StreamChunk, ToolSchema};
    use std::sync::{Arc, Mutex};

    struct ScriptedProvider {
        response: Mutex<Result<String, AgentError>>,
        recorded: Mutex<Vec<RecordedTitleCall>>,
    }

    #[derive(Clone, Debug)]
    struct RecordedTitleCall {
        messages: Vec<Message>,
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<String>,
    }

    impl ScriptedProvider {
        fn with_response(text: &str) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Ok(text.to_string())),
                recorded: Mutex::new(Vec::new()),
            })
        }

        fn with_error(message: &str) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Err(AgentError::LlmApi(message.to_string()))),
                recorded: Mutex::new(Vec::new()),
            })
        }

        fn recorded(&self) -> Vec<RecordedTitleCall> {
            self.recorded.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            max_tokens: Option<u32>,
            temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            self.recorded.lock().unwrap().push(RecordedTitleCall {
                messages: messages.to_vec(),
                max_tokens,
                temperature,
                model: model.map(ToOwned::to_owned),
            });

            let response = self.response.lock().unwrap().clone()?;
            Ok(LlmResponse {
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(response),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
                usage: None,
                model: model.unwrap_or("test-title-model").to_string(),
                finish_reason: Some("stop".into()),
                response_id: None,
                dropped_tool_names: None,
                rate_limit_headers: None,
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            panic!("ScriptedProvider streaming is not used by title tests")
        }
    }

    #[test]
    fn test_truncate_messages() {
        let generator = TitleGenerator::for_test();
        let messages = vec![
            Message::user("Hello, can you help me with Rust programming?"),
            Message::assistant("Of course! I'd be happy to help with Rust."),
        ];
        let result = generator.truncate_messages(&messages);
        assert!(result.contains("User:"));
        assert!(result.contains("Assistant:"));
    }

    #[test]
    fn test_truncate_long_messages() {
        let generator = TitleGenerator::for_test().with_max_message_chars(20);
        let messages = vec![Message::user(
            "This is a very long message that should be truncated",
        )];
        let result = generator.truncate_messages(&messages);
        // The truncated content should be at most 20 chars
        let content_line = result.split(':').nth(1).unwrap().trim();
        assert!(content_line.len() <= 20);
    }

    #[test]
    fn test_empty_messages_error() {
        let generator = TitleGenerator::for_test();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(generator.generate_title(&[]));
        assert!(matches!(result, Err(TitleError::NoMessages)));
    }

    #[test]
    fn test_generate_title_strips_quotes() {
        let provider = ScriptedProvider::with_response("\"Setting Up Docker Environment\"");
        let generator = TitleGenerator::new(provider, "title-model");
        let rt = tokio::runtime::Runtime::new().unwrap();

        let title = rt
            .block_on(generator.generate_title(&[
                Message::user("how do I set up docker"),
                Message::assistant("First install Docker Desktop."),
            ]))
            .unwrap();

        assert_eq!(title, "Setting Up Docker Environment");
    }

    #[test]
    fn test_generate_title_strips_title_prefix() {
        let provider = ScriptedProvider::with_response("Title: Kubernetes Pod Debugging");
        let generator = TitleGenerator::new(provider, "title-model");
        let rt = tokio::runtime::Runtime::new().unwrap();

        let title = rt
            .block_on(generator.generate_title(&[
                Message::user("my pod keeps crashing"),
                Message::assistant("Let me inspect the logs."),
            ]))
            .unwrap();

        assert_eq!(title, "Kubernetes Pod Debugging");
    }

    #[test]
    fn test_generate_title_truncates_long_titles() {
        let provider = ScriptedProvider::with_response(&"A".repeat(100));
        let generator = TitleGenerator::new(provider, "title-model");
        let rt = tokio::runtime::Runtime::new().unwrap();

        let title = rt
            .block_on(
                generator
                    .generate_title(&[Message::user("question"), Message::assistant("answer")]),
            )
            .unwrap();

        assert_eq!(title.chars().count(), 80);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_generate_title_returns_empty_result_for_empty_response() {
        let provider = ScriptedProvider::with_response("");
        let generator = TitleGenerator::new(provider, "title-model");
        let rt = tokio::runtime::Runtime::new().unwrap();

        let result = rt.block_on(
            generator.generate_title(&[Message::user("question"), Message::assistant("answer")]),
        );

        assert!(matches!(result, Err(TitleError::EmptyResult)));
    }

    #[test]
    fn test_generate_title_surfaces_provider_errors() {
        let provider = ScriptedProvider::with_error("no provider");
        let generator = TitleGenerator::new(provider, "title-model");
        let rt = tokio::runtime::Runtime::new().unwrap();

        let result = rt.block_on(
            generator.generate_title(&[Message::user("question"), Message::assistant("answer")]),
        );

        assert!(
            matches!(result, Err(TitleError::LlmError(message)) if message.contains("no provider"))
        );
    }

    #[test]
    fn test_generate_title_request_uses_small_title_budget_and_truncated_prompt() {
        let provider = ScriptedProvider::with_response("Short Title");
        let generator =
            TitleGenerator::new(provider.clone(), "title-model").with_max_message_chars(20);
        let rt = tokio::runtime::Runtime::new().unwrap();

        let title = rt
            .block_on(generator.generate_title(&[
                Message::user("x".repeat(1000)),
                Message::assistant("y".repeat(1000)),
            ]))
            .unwrap();

        assert_eq!(title, "Short Title");
        let calls = provider.recorded();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].max_tokens, Some(32));
        assert_eq!(calls[0].temperature, Some(0.3));
        assert_eq!(calls[0].model.as_deref(), Some("title-model"));
        let user_prompt = calls[0].messages[1].content.as_deref().unwrap();
        assert!(user_prompt.contains("User:"));
        assert!(user_prompt.contains("Assistant:"));
        assert!(user_prompt.len() < 100);
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
                    _tools: &[hermes_core::ToolSchema],
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
                    _tools: &[hermes_core::ToolSchema],
                    _max_tokens: Option<u32>,
                    _temperature: Option<f64>,
                    _model: Option<&str>,
                    _extra_body: Option<&serde_json::Value>,
                ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
                {
                    panic!("MockProvider should not be called in this test")
                }
            }
            Self {
                llm_provider: Arc::new(MockProvider),
                model: "test-model".to_string(),
                max_messages: 10,
                max_message_chars: 200,
            }
        }
    }
}
