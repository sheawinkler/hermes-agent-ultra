//! Per-turn cache for LLM request serialization (OpenAI-compat sanitize + Anthropic convert).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use hermes_core::{Message, MessageRole, ToolCall, ToolSchema};
use serde_json::Value;

use crate::provider::{AnthropicProvider, GenericProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MessagesCacheKey {
    count: usize,
    content_hash: u64,
    strict: bool,
    model_hash: u64,
    profile_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnthropicMessagesCacheKey {
    pub count: usize,
    pub content_hash: u64,
    pub base_url_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolsCacheKey {
    count: usize,
    schema_hash: u64,
}

type AnthropicConverted = (Option<Value>, Vec<Value>);

/// Turn-scoped cache shared across runtime-built provider instances.
#[derive(Debug)]
pub(crate) struct ProviderSerializeCache {
    messages: Mutex<Option<(MessagesCacheKey, Arc<Value>)>>,
    tools: Mutex<Option<(ToolsCacheKey, Arc<Value>)>>,
    anthropic_messages: Mutex<Option<(AnthropicMessagesCacheKey, Arc<AnthropicConverted>)>>,
    anthropic_tools: Mutex<Option<(ToolsCacheKey, Arc<Value>)>>,
}

impl ProviderSerializeCache {
    pub fn new() -> Self {
        Self {
            messages: Mutex::new(None),
            tools: Mutex::new(None),
            anthropic_messages: Mutex::new(None),
            anthropic_tools: Mutex::new(None),
        }
    }

    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.messages.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.tools.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.anthropic_messages.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.anthropic_tools.lock() {
            *guard = None;
        }
    }

    pub fn sanitized_openai_messages(
        &self,
        messages: &[Message],
        strict: bool,
        effective_model: &str,
        profile: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Value {
        let key = messages_cache_key(messages, strict, effective_model, profile);
        if let Ok(mut guard) = self.messages.lock() {
            if let Some((cached_key, arc)) = guard.as_ref() {
                if *cached_key == key {
                    return Arc::clone(arc).as_ref().clone();
                }
            }
            let value = GenericProvider::sanitize_messages_for_api(
                messages,
                strict,
                effective_model,
                profile,
                extra_body,
            );
            let arc = Arc::new(value);
            let out = arc.as_ref().clone();
            *guard = Some((key, arc));
            return out;
        }
        GenericProvider::sanitize_messages_for_api(
            messages,
            strict,
            effective_model,
            profile,
            extra_body,
        )
    }

    pub fn formatted_openai_tools(&self, tools: &[ToolSchema]) -> Value {
        if tools.is_empty() {
            return Value::Array(vec![]);
        }
        let key = tools_cache_key(tools);
        if let Ok(mut guard) = self.tools.lock() {
            if let Some((cached_key, arc)) = guard.as_ref() {
                if *cached_key == key {
                    return Arc::clone(arc).as_ref().clone();
                }
            }
            let value = GenericProvider::format_tools_for_openai_api(tools);
            let arc = Arc::new(value);
            let out = arc.as_ref().clone();
            *guard = Some((key, arc));
            return out;
        }
        GenericProvider::format_tools_for_openai_api(tools)
    }

    pub fn converted_anthropic_messages(
        &self,
        messages: &[Message],
        base_url: &str,
    ) -> (Option<Value>, Vec<Value>) {
        let key = anthropic_messages_cache_key(messages, base_url);
        if let Ok(mut guard) = self.anthropic_messages.lock() {
            if let Some((cached_key, arc)) = guard.as_ref() {
                if *cached_key == key {
                    let (system, msgs) = arc.as_ref();
                    return (system.clone(), msgs.clone());
                }
            }
            let converted = AnthropicProvider::convert_messages(messages, Some(base_url));
            let arc = Arc::new(converted);
            let out = arc.as_ref().clone();
            *guard = Some((key, arc));
            return out;
        }
        AnthropicProvider::convert_messages(messages, Some(base_url))
    }

    pub fn formatted_anthropic_tools(&self, tools: &[ToolSchema]) -> Value {
        if tools.is_empty() {
            return Value::Array(vec![]);
        }
        let key = tools_cache_key_for_tools(tools);
        if let Ok(mut guard) = self.anthropic_tools.lock() {
            if let Some((cached_key, arc)) = guard.as_ref() {
                if *cached_key == key {
                    return arc.as_ref().clone();
                }
            }
            let value = Value::Array(AnthropicProvider::convert_tools(tools));
            let arc = Arc::new(value);
            let out = arc.as_ref().clone();
            *guard = Some((key, arc));
            return out;
        }
        Value::Array(AnthropicProvider::convert_tools(tools))
    }
}

pub(crate) fn anthropic_messages_cache_key(messages: &[Message], base_url: &str) -> AnthropicMessagesCacheKey {
    let content = messages_cache_key(messages, false, "", None);
    AnthropicMessagesCacheKey {
        count: content.count,
        content_hash: content.content_hash,
        base_url_hash: hash_str(base_url),
    }
}

pub(crate) fn tools_cache_key_for_tools(tools: &[ToolSchema]) -> ToolsCacheKey {
    tools_cache_key(tools)
}

pub(crate) fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn messages_cache_key(
    messages: &[Message],
    strict: bool,
    effective_model: &str,
    profile: Option<&str>,
) -> MessagesCacheKey {
    let mut hasher = DefaultHasher::new();
    strict.hash(&mut hasher);
    effective_model.hash(&mut hasher);
    if let Some(profile) = profile {
        profile.hash(&mut hasher);
    }
    messages.len().hash(&mut hasher);
    for msg in messages {
        hash_message(msg, &mut hasher);
    }
    MessagesCacheKey {
        count: messages.len(),
        content_hash: hasher.finish(),
        strict,
        model_hash: hash_str(effective_model),
        profile_hash: profile.map(hash_str).unwrap_or(0),
    }
}

fn hash_message(msg: &Message, hasher: &mut DefaultHasher) {
    hash_message_role(msg.role, hasher);
    if let Some(content) = &msg.content {
        content.hash(hasher);
    }
    if let Some(id) = &msg.tool_call_id {
        id.hash(hasher);
    }
    if let Some(name) = &msg.name {
        name.hash(hasher);
    }
    if let Some(reasoning) = &msg.reasoning_content {
        reasoning.hash(hasher);
    }
    if let Some(cc) = &msg.cache_control {
        cc.ttl.hash(hasher);
        let cache_type = match cc.cache_type {
            hermes_core::CacheType::Ephemeral => 0u8,
            hermes_core::CacheType::Persistent => 1,
        };
        cache_type.hash(hasher);
    }
    if let Some(calls) = &msg.tool_calls {
        calls.len().hash(hasher);
        for tc in calls {
            hash_tool_call(tc, hasher);
        }
    }
}

fn hash_message_role(role: MessageRole, hasher: &mut DefaultHasher) {
    let tag = match role {
        MessageRole::System => 0u8,
        MessageRole::User => 1,
        MessageRole::Assistant => 2,
        MessageRole::Tool => 3,
    };
    tag.hash(hasher);
}

fn hash_tool_call(tc: &ToolCall, hasher: &mut DefaultHasher) {
    tc.id.hash(hasher);
    tc.function.name.hash(hasher);
    tc.function.arguments.hash(hasher);
    if let Some(extra) = &tc.extra_content {
        extra.to_string().hash(hasher);
    }
}

fn tools_cache_key(tools: &[ToolSchema]) -> ToolsCacheKey {
    let mut hasher = DefaultHasher::new();
    tools.len().hash(&mut hasher);
    for tool in tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        if let Ok(params) = serde_json::to_string(&tool.parameters) {
            params.hash(&mut hasher);
        }
    }
    ToolsCacheKey {
        count: tools.len(),
        schema_hash: hasher.finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, JsonSchema, Message};

    #[test]
    fn cache_returns_identical_sanitized_messages() {
        let cache = ProviderSerializeCache::new();
        let messages = vec![
            Message::system("sys"),
            Message::user("hello"),
            Message::assistant_with_tool_calls(
                None,
                vec![ToolCall {
                    id: "c1".into(),
                    function: FunctionCall {
                        name: "read".into(),
                        arguments: "{}".into(),
                    },
                    extra_content: None,
                }],
            ),
        ];
        let a = cache.sanitized_openai_messages(&messages, true, "gpt-4o", None, None);
        let b = cache.sanitized_openai_messages(&messages, true, "gpt-4o", None, None);
        assert_eq!(a, b);
    }

    #[test]
    fn anthropic_cache_returns_identical_converted_messages() {
        let cache = ProviderSerializeCache::new();
        let messages = vec![
            Message::system("sys"),
            Message::user("hello"),
        ];
        let a = cache.converted_anthropic_messages(&messages, "https://api.anthropic.com");
        let b = cache.converted_anthropic_messages(&messages, "https://api.anthropic.com");
        assert_eq!(a, b);
    }

    #[test]
    fn cache_invalidates_tools_after_schema_change() {
        let cache = ProviderSerializeCache::new();
        let empty_schema = JsonSchema::new("object");
        let t1 = vec![ToolSchema::new("a", "desc", empty_schema.clone())];
        let first = cache.formatted_openai_tools(&t1);
        let t2 = vec![ToolSchema::new("b", "desc", empty_schema)];
        let second = cache.formatted_openai_tools(&t2);
        assert_ne!(first, second);
    }
}
