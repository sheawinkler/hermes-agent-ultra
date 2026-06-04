//! Anthropic prompt caching — parity with Python `agent/prompt_caching.py`.
//!
//! Layout: `system_and_3` — up to 4 breakpoints (system + last 3 non-system messages).

use hermes_core::types::{CacheControl, CacheType, Message, MessageRole};

/// Build a cache marker for the given TTL tier (`"5m"` or `"1h"`).
pub fn build_cache_marker(cache_ttl: &str) -> CacheControl {
    let ttl = if cache_ttl == "1h" {
        Some("1h".to_string())
    } else {
        None
    };
    CacheControl {
        cache_type: CacheType::Ephemeral,
        ttl,
    }
}

/// Decide whether to apply Anthropic prompt caching and which layout to use.
///
/// Returns `(should_cache, use_native_layout)` — mirrors Python `anthropic_prompt_cache_policy`.
pub fn anthropic_prompt_cache_policy(
    provider: &str,
    base_url: &str,
    api_mode: &str,
    model: &str,
) -> (bool, bool) {
    crate::agent_runtime_helpers::anthropic_prompt_cache_policy(provider, base_url, api_mode, model)
}

fn base_url_hostname(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .split("://")
        .nth(1)
        .unwrap_or(trimmed);
    without_scheme
        .split('/')
        .next()
        .map(|host| host.split(':').next().unwrap_or(host).to_ascii_lowercase())
}

fn base_url_host_matches(base_url: &str, host: &str) -> bool {
    base_url_hostname(base_url)
        .as_deref()
        .is_some_and(|h| h == host || h.ends_with(&format!(".{host}")))
}

fn apply_cache_marker(msg: &mut Message, marker: &CacheControl, native_anthropic: bool) {
    if msg.role == MessageRole::Tool {
        if native_anthropic {
            msg.cache_control = Some(marker.clone());
        }
        return;
    }
    let empty = msg
        .content
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if empty {
        msg.cache_control = Some(marker.clone());
        return;
    }
    msg.cache_control = Some(marker.clone());
}

/// Apply `system_and_3` caching strategy in place (no extra message vector).
pub fn apply_anthropic_cache_control_in_place(
    messages: &mut [Message],
    cache_ttl: &str,
    native_anthropic: bool,
) {
    if messages.is_empty() {
        return;
    }

    let marker = build_cache_marker(cache_ttl);
    let mut breakpoints_used = 0usize;

    if messages.first().is_some_and(|m| m.role == MessageRole::System) {
        apply_cache_marker(&mut messages[0], &marker, native_anthropic);
        breakpoints_used += 1;
    }

    let remaining = 4usize.saturating_sub(breakpoints_used);
    let non_sys: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role != MessageRole::System)
        .map(|(i, _)| i)
        .collect();
    for idx in non_sys.into_iter().rev().take(remaining) {
        apply_cache_marker(&mut messages[idx], &marker, native_anthropic);
    }
}

/// Apply `system_and_3` caching strategy (deep-copied messages).
pub fn apply_anthropic_cache_control(
    api_messages: &[Message],
    cache_ttl: &str,
    native_anthropic: bool,
) -> Vec<Message> {
    let mut messages: Vec<Message> = api_messages.to_vec();
    apply_anthropic_cache_control_in_place(&mut messages, cache_ttl, native_anthropic);
    messages
}

/// Record Prometheus prompt-cache telemetry from a raw provider usage object.
pub fn record_prompt_cache_telemetry(raw_usage: &serde_json::Value) {
    let cache_read = raw_usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write = raw_usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if cache_read > 0 {
        hermes_telemetry::record_prompt_cache_hit();
    }
    if cache_write > 0 {
        hermes_telemetry::record_prompt_cache_miss();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cache_marker_5m_has_no_ttl() {
        let m = build_cache_marker("5m");
        assert_eq!(m.cache_type, CacheType::Ephemeral);
        assert!(m.ttl.is_none());
        assert_eq!(m.to_api_json(), serde_json::json!({"type": "ephemeral"}));
    }

    #[test]
    fn build_cache_marker_1h_sets_ttl() {
        let m = build_cache_marker("1h");
        assert_eq!(
            m.to_api_json(),
            serde_json::json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn in_place_matches_copy_path() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
        ];
        let mut in_place = msgs.clone();
        apply_anthropic_cache_control_in_place(&mut in_place, "5m", true);
        let copied = apply_anthropic_cache_control(&msgs, "5m", true);
        assert_eq!(
            in_place
                .iter()
                .map(|m| m.cache_control.is_some())
                .collect::<Vec<_>>(),
            copied
                .iter()
                .map(|m| m.cache_control.is_some())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn system_and_3_marks_system_plus_last_three_non_system() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
            Message::user("u3"),
        ];
        let out = apply_anthropic_cache_control(&msgs, "5m", true);
        assert!(out[0].cache_control.is_some());
        assert!(out[3].cache_control.is_some());
        assert!(out[4].cache_control.is_some());
        assert!(out[5].cache_control.is_some());
        assert!(out[1].cache_control.is_none());
        assert!(out[2].cache_control.is_none());
    }

    #[test]
    fn tool_cache_only_when_native() {
        let mut tool = Message {
            role: MessageRole::Tool,
            content: Some("ok".into()),
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
        };
        let marker = build_cache_marker("5m");
        apply_cache_marker(&mut tool, &marker, false);
        assert!(tool.cache_control.is_none());
        apply_cache_marker(&mut tool, &marker, true);
        assert!(tool.cache_control.is_some());
    }

    #[test]
    fn policy_openrouter_claude_envelope() {
        let (cache, native) = anthropic_prompt_cache_policy(
            "openrouter",
            "https://openrouter.ai/api/v1",
            "chat_completions",
            "anthropic/claude-sonnet-4",
        );
        assert!(cache);
        assert!(!native);
    }

    #[test]
    fn policy_native_anthropic() {
        let (cache, native) = anthropic_prompt_cache_policy(
            "anthropic",
            "https://api.anthropic.com",
            "anthropic_messages",
            "claude-sonnet-4-20250514",
        );
        assert!(cache);
        assert!(native);
    }
}
