//! Inbound message preparation trait (gateway → agent orchestration).

use async_trait::async_trait;

use crate::{AgentError, Message};

/// Platform-agnostic inbound event for preparation (mirrors gateway `IncomingMessage`).
#[derive(Debug, Clone)]
pub struct InboundEvent {
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
    pub media_urls: Vec<String>,
    pub media_types: Vec<String>,
    pub message_id: Option<String>,
    pub is_dm: bool,
}

/// Runtime context for inbound preparation.
#[derive(Debug, Clone, Default)]
pub struct InboundPrepareContext {
    pub session_key: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// `auto` | `native` | `text`
    pub image_input_mode: String,
    pub aux_vision_provider: Option<String>,
    pub aux_vision_model: Option<String>,
    pub aux_vision_base_url: Option<String>,
}

/// Prepares a user `Message` from a platform inbound event (vision routing, transcription, etc.).
#[async_trait]
pub trait InboundMessagePreparer: Send + Sync {
    async fn prepare(
        &self,
        event: &InboundEvent,
        ctx: &InboundPrepareContext,
    ) -> Result<Message, AgentError>;
}

/// Transport-only fallback: format media paths without LLM enrichment.
pub fn transport_fallback_message(event: &InboundEvent) -> Message {
    if event.media_urls.is_empty() {
        return Message::user(event.text.clone());
    }
    let mut media_lines = Vec::new();
    for (idx, url) in event.media_urls.iter().enumerate() {
        let url = url.trim();
        if url.is_empty() {
            continue;
        }
        let media_type = event
            .media_types
            .get(idx)
            .map(String::as_str)
            .unwrap_or("file")
            .trim();
        media_lines.push(format!("[media:{media_type}] {url}"));
    }
    if media_lines.is_empty() {
        return Message::user(event.text.clone());
    }
    let body = if event.text.trim().is_empty() {
        media_lines.join("\n")
    } else {
        format!("{}\n{}", event.text, media_lines.join("\n"))
    };
    Message::user(body)
}
