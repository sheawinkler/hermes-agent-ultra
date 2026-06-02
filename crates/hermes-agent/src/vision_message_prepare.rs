//! Strip multimodal parts for non-vision models — parity with Python `_prepare_messages_for_non_vision_model`.

use hermes_core::{Message, MessageRole};

/// Known vision-capable model id substrings (heuristic; extend via config later).
pub fn model_supports_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    [
        "gpt-4o",
        "gpt-4.1",
        "gpt-5",
        "claude-3",
        "claude-sonnet-4",
        "claude-opus-4",
        "gemini",
        "pixtral",
        "llava",
        "qwen-vl",
        "vision",
    ]
    .iter()
    .any(|hint| m.contains(hint))
}

/// Return copies of messages with image parts removed when the model lacks vision.
pub fn strip_images_for_non_vision_model(messages: &[Message], model: &str) -> Vec<Message> {
    if model_supports_vision(model) {
        return messages.to_vec();
    }
    let mut out = messages.to_vec();
    strip_images_for_non_vision_model_in_place(&mut out);
    out
}

/// In-place variant — avoids a second full vector allocation.
pub fn strip_images_for_non_vision_model_in_place(messages: &mut [Message]) {
    const PLACEHOLDER: &str = "[Image content removed: active model does not support vision. \
         Describe the image in text or switch to a vision-capable model.]";
    for msg in messages.iter_mut() {
        if !matches!(msg.role, MessageRole::User | MessageRole::Tool) {
            continue;
        }
        let Some(content) = msg.content.as_deref() else {
            continue;
        };
        if content.contains("data:image") || content.contains("\"type\":\"image") {
            msg.content = Some(PLACEHOLDER.to_string());
        }
    }
}
