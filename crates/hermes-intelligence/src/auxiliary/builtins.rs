//! Convenience builders for the most common auxiliary tasks.
//!
//! These wrap [`AuxiliaryClient`](super::client::AuxiliaryClient) with the
//! prompt scaffolding that the Python baseline embeds in each call site, so
//! Rust callers can simply pass user content rather than re-implementing the
//! system prompt for every task.

use hermes_core::{Message, MessageRole};

use super::client::AuxiliaryRequest;
use super::task::AuxiliaryTask;

fn system(text: impl Into<String>) -> Message {
    Message {
        role: MessageRole::System,
        content: Some(text.into()),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
    }
}

fn user(text: impl Into<String>) -> Message {
    Message {
        role: MessageRole::User,
        content: Some(text.into()),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
    }
}

/// Build a title-generation request from a snippet of conversation.
///
/// Mirrors the Python `agent/title_generator.py` prompt — a tight system
/// instruction plus the trimmed conversation as user content.
pub fn build_title_request(conversation: &str) -> AuxiliaryRequest {
    let messages = vec![
        system(
            "You write concise conversation titles. \
             Reply with a single line of at most 6 words. \
             No quotes, no trailing punctuation, no emojis.",
        ),
        user(format!(
            "Generate a short title (≤6 words) for this conversation:\n\n{conversation}"
        )),
    ];
    AuxiliaryRequest::new(AuxiliaryTask::Title, messages)
}

/// Build a context-compression request — instructs the model to compress a
/// conversation window into a dense summary while preserving open questions
/// and pending action items.
pub fn build_compression_request(content: &str) -> AuxiliaryRequest {
    let messages = vec![
        system(
            "You compress conversation history into dense, faithful summaries. \
             Preserve: (1) open user questions, (2) decisions that have already \
             been made, (3) any pending action items. Drop pleasantries and \
             redundant tool output. Reply with the summary only — no prefix, no \
             commentary.",
        ),
        user(content.to_string()),
    ];
    AuxiliaryRequest::new(AuxiliaryTask::Compression, messages)
}

/// Build a free-form classification request — `prompt` describes the
/// classification scheme; `input` is the text to classify. The model is told
/// to reply with a single label.
pub fn build_classify_request(scheme: &str, input: &str) -> AuxiliaryRequest {
    let messages = vec![
        system(format!(
            "You are a deterministic classifier. {scheme}\nReply with the \
             single best label only — no explanation, no quotes, no \
             punctuation."
        )),
        user(input.to_string()),
    ];
    AuxiliaryRequest::new(AuxiliaryTask::Classify, messages)
}

/// Build a web extraction request — given a fetched page's raw text, ask the
/// model to extract the readable article content with markdown headings.
pub fn build_web_extract_request(raw_html_or_text: &str) -> AuxiliaryRequest {
    let messages = vec![
        system(
            "You extract the main readable article from raw web page content. \
             Strip nav menus, ads, footers, sidebars. Preserve headings, \
             paragraphs, lists, and code blocks. Reply with the cleaned \
             markdown only — no commentary.",
        ),
        user(raw_html_or_text.to_string()),
    ];
    AuxiliaryRequest::new(AuxiliaryTask::WebExtract, messages)
}

/// Build a session-search request — given the user's query and a list of
/// candidate snippets, ask the model to pick the most relevant ones.
pub fn build_session_search_request(query: &str, snippets: &[String]) -> AuxiliaryRequest {
    let mut snippet_block = String::new();
    for (i, s) in snippets.iter().enumerate() {
        snippet_block.push_str(&format!("[{}] {}\n\n", i, s));
    }
    let messages = vec![
        system(
            "You select the most relevant snippets for a user query from a \
             numbered candidate list. Reply with a comma-separated list of \
             the chosen indices in order of relevance. Reply with `none` if \
             no candidate is relevant.",
        ),
        user(format!("Query: {query}\n\nCandidates:\n{snippet_block}")),
    ];
    AuxiliaryRequest::new(AuxiliaryTask::SessionSearch, messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_request_carries_task_and_user_text() {
        let req = build_title_request("Alice: hi\nBob: hello");
        assert_eq!(req.task, Some(AuxiliaryTask::Title));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, MessageRole::System);
        assert_eq!(req.messages[1].role, MessageRole::User);
        assert!(req.messages[1]
            .content
            .as_deref()
            .unwrap()
            .contains("Alice"));
    }

    #[test]
    fn classify_request_inlines_scheme() {
        let req = build_classify_request(
            "Classify intent as one of: ask, command, idle.",
            "what time?",
        );
        let sys = req.messages[0].content.as_deref().unwrap();
        assert!(sys.contains("Classify intent"));
    }
}
