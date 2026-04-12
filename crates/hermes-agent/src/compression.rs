//! LLM-assisted context compression helpers.

use hermes_core::{AgentError, LlmProvider, Message};

/// Build a compact summary text from older messages via an LLM provider.
///
/// This is a lightweight version of Python's summarization compression:
/// - keeps caller in control of which message slice is compressed
/// - asks the active model to output concise bullet points
pub async fn summarize_messages_with_llm(
    provider: &dyn LlmProvider,
    messages: &[Message],
    model: Option<&str>,
) -> Result<String, AgentError> {
    let mut prompt_messages = Vec::with_capacity(2 + messages.len());
    prompt_messages.push(Message::system(
        "Summarize the conversation into concise bullets. Preserve facts, decisions, todos, file paths, and unresolved questions.",
    ));
    prompt_messages.push(Message::user(
        "Return only the summary text. Keep it under 3000 characters.",
    ));
    prompt_messages.extend_from_slice(messages);

    let resp = provider
        .chat_completion(
            &prompt_messages,
            &[],
            Some(700),
            Some(0.1),
            model,
            None,
        )
        .await?;

    Ok(resp
        .message
        .content
        .unwrap_or_else(|| "[summary unavailable]".to_string()))
}
