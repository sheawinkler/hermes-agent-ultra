//! API-bound message assembly (optimized path). Legacy baseline lives on [`crate::agent_loop::AgentLoop`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use hermes_core::{Message, MessageRole};

use crate::memory_manager::build_memory_context_block;
use crate::prompt_caching::apply_anthropic_cache_control_in_place;
use crate::vision_message_prepare::{
    model_supports_vision, strip_images_for_non_vision_model_in_place,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApiMessagesCacheKey {
    pub message_count: usize,
    pub total_chars: usize,
    pub prefetch_len: usize,
    pub prefetch_hash: u64,
    pub ephemeral_len: usize,
    pub model_hash: u64,
    pub use_prompt_caching: bool,
    pub use_native_cache_layout: bool,
    pub cache_ttl_hash: u64,
}

pub(crate) fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

pub(crate) fn apply_prefetch_to_last_user(messages: &mut [Message], prefetch: &str) {
    if prefetch.trim().is_empty() {
        return;
    }
    let Some(idx) = messages.iter().rposition(|m| m.role == MessageRole::User) else {
        return;
    };
    let fenced = build_memory_context_block(prefetch);
    if fenced.is_empty() {
        return;
    }
    if let Some(msg) = messages.get_mut(idx) {
        let base = msg.content.clone().unwrap_or_default();
        msg.content = Some(format!("{base}\n\n{fenced}"));
    }
}

pub(crate) fn assemble_api_messages_from_ctx(
    source: &[Message],
    prefetch: &str,
    ephemeral: Option<&str>,
    model: &str,
    cache_ttl: &str,
    use_prompt_caching: bool,
    use_native_cache_layout: bool,
) -> Vec<Message> {
    let last_user_idx = source
        .iter()
        .rposition(|m| m.role == MessageRole::User);
    let fenced = if prefetch.trim().is_empty() {
        String::new()
    } else {
        build_memory_context_block(prefetch)
    };
    let merge_prefetch = !fenced.is_empty() && last_user_idx.is_some();

    let extra = ephemeral.is_some() as usize;
    let mut out = Vec::with_capacity(source.len() + extra);

    for (i, msg) in source.iter().enumerate() {
        if merge_prefetch && last_user_idx == Some(i) {
            let mut merged = msg.clone();
            let base = merged.content.take().unwrap_or_default();
            merged.content = Some(format!("{base}\n\n{fenced}"));
            out.push(merged);
        } else {
            out.push(msg.clone());
        }
    }

    if let Some(ephemeral) = ephemeral {
        out.push(Message::system(ephemeral));
    }

    if !out.is_empty() && use_prompt_caching {
        apply_anthropic_cache_control_in_place(&mut out, cache_ttl, use_native_cache_layout);
    }

    if model_supports_vision(model) {
        return out;
    }
    strip_images_for_non_vision_model_in_place(&mut out);
    out
}
