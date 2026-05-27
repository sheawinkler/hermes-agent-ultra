//! Canonical JSON shapes for gateway hook `context` payloads.
//!
//! Builders use stable field sets; `serde_json::Map` serializes keys in
//! **lexicographic order**, which we assert in tests so drift is caught.

use serde_json::{json, Value};

use crate::gateway::IncomingMessage;

/// `gateway:startup`
pub fn gateway_startup(enabled_platforms: &[impl AsRef<str>]) -> Value {
    let v: Vec<String> = enabled_platforms
        .iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    json!({ "enabled_platforms": v })
}

/// `session:start`
pub fn session_start(
    platform: &str,
    chat_id: &str,
    user_id: &str,
    session_id: &str,
    reason: &str,
) -> Value {
    json!({
        "platform": platform,
        "chat_id": chat_id,
        "user_id": user_id,
        "session_id": session_id,
        "reason": reason,
    })
}

/// `session:start` from an incoming message.
pub fn session_start_from_incoming(
    incoming: &IncomingMessage,
    session_id: &str,
    reason: &str,
) -> Value {
    session_start(
        &incoming.platform,
        &incoming.chat_id,
        &incoming.user_id,
        session_id,
        reason,
    )
}

/// `session:end` / `session:reset` base shape (same keys).
pub fn session_lifecycle(platform: &str, chat_id: &str, user_id: &str, session_id: &str) -> Value {
    json!({
        "platform": platform,
        "chat_id": chat_id,
        "user_id": user_id,
        "session_id": session_id,
    })
}

pub fn session_lifecycle_from_incoming(incoming: &IncomingMessage, session_id: &str) -> Value {
    session_lifecycle(
        &incoming.platform,
        &incoming.chat_id,
        &incoming.user_id,
        session_id,
    )
}

/// `command:<name>` — `text` is the full user slash line (trimmed), for Python parity.
pub fn command_context(
    incoming: &IncomingMessage,
    session_id: &str,
    command_name: &str,
    command_line: &str,
) -> Value {
    json!({
        "platform": incoming.platform,
        "chat_id": incoming.chat_id,
        "user_id": incoming.user_id,
        "session_id": session_id,
        "command": command_name,
        "text": command_line,
    })
}

/// `agent:start`
pub fn agent_start(incoming: &IncomingMessage, session_id: &str, streaming: bool) -> Value {
    json!({
        "platform": incoming.platform,
        "chat_id": incoming.chat_id,
        "user_id": incoming.user_id,
        "session_id": session_id,
        "streaming": streaming,
    })
}

/// `agent:end` success path
pub fn agent_end_success(
    incoming: &IncomingMessage,
    session_id: &str,
    streaming: bool,
    response_chars: usize,
) -> Value {
    json!({
        "platform": incoming.platform,
        "chat_id": incoming.chat_id,
        "user_id": incoming.user_id,
        "session_id": session_id,
        "streaming": streaming,
        "success": true,
        "response_chars": response_chars,
    })
}

/// `agent:end` error path
pub fn agent_end_error(
    incoming: &IncomingMessage,
    session_id: &str,
    streaming: bool,
    error: &str,
) -> Value {
    json!({
        "platform": incoming.platform,
        "chat_id": incoming.chat_id,
        "user_id": incoming.user_id,
        "session_id": session_id,
        "streaming": streaming,
        "success": false,
        "error": error,
    })
}

/// `agent:status` — lifecycle / pressure notifications from the agent loop.
pub fn agent_status(
    platform: impl AsRef<str>,
    chat_id: impl AsRef<str>,
    user_id: impl AsRef<str>,
    session_id: impl AsRef<str>,
    event_type: &str,
    message: &str,
) -> Value {
    json!({
        "platform": platform.as_ref(),
        "chat_id": chat_id.as_ref(),
        "user_id": user_id.as_ref(),
        "session_id": session_id.as_ref(),
        "event_type": event_type,
        "message": message,
    })
}

/// `agent:step` — one tool-calling iteration summary.
pub fn agent_step(
    platform: impl AsRef<str>,
    chat_id: impl AsRef<str>,
    user_id: impl AsRef<str>,
    session_id: impl AsRef<str>,
    iteration: u32,
    tool_names: Vec<String>,
    tools: Vec<Value>,
) -> Value {
    json!({
        "platform": platform.as_ref(),
        "chat_id": chat_id.as_ref(),
        "user_id": user_id.as_ref(),
        "session_id": session_id.as_ref(),
        "iteration": iteration,
        "tool_names": tool_names,
        "tools": tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn golden_key_order_gateway_startup() {
        let v = gateway_startup(&["telegram", "discord"]);
        assert_eq!(object_keys(&v), vec!["enabled_platforms"]);
        assert_eq!(v["enabled_platforms"], json!(["telegram", "discord"]));
    }

    #[test]
    fn golden_key_order_session_start() {
        let v = session_start("p", "c", "u", "sid", "new");
        assert_eq!(
            object_keys(&v),
            vec!["chat_id", "platform", "reason", "session_id", "user_id"]
        );
    }

    #[test]
    fn golden_key_order_agent_start_end() {
        let incoming = IncomingMessage {
            platform: "tg".into(),
            chat_id: "ch".into(),
            user_id: "u1".into(),
            text: "".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
        };
        let s = agent_start(&incoming, "sk", true);
        assert_eq!(
            object_keys(&s),
            vec!["chat_id", "platform", "session_id", "streaming", "user_id"]
        );
        let ok = agent_end_success(&incoming, "sk", true, 42);
        assert_eq!(
            object_keys(&ok),
            vec![
                "chat_id",
                "platform",
                "response_chars",
                "session_id",
                "streaming",
                "success",
                "user_id"
            ]
        );
        let err = agent_end_error(&incoming, "sk", false, "boom");
        assert_eq!(
            object_keys(&err),
            vec![
                "chat_id",
                "error",
                "platform",
                "session_id",
                "streaming",
                "success",
                "user_id"
            ]
        );
    }

    #[test]
    fn golden_key_order_agent_status_step() {
        let st = agent_status("p", "c", "u", "sid", "lifecycle", "85%");
        assert_eq!(
            object_keys(&st),
            vec![
                "chat_id",
                "event_type",
                "message",
                "platform",
                "session_id",
                "user_id"
            ]
        );
        let step = agent_step(
            "p",
            "c",
            "u",
            "sid",
            3,
            vec!["memory".into()],
            vec![json!({"name":"memory","result":"ok"})],
        );
        assert_eq!(
            object_keys(&step),
            vec![
                "chat_id",
                "iteration",
                "platform",
                "session_id",
                "tool_names",
                "tools",
                "user_id"
            ]
        );
    }

    #[test]
    fn golden_key_order_command() {
        let incoming = IncomingMessage {
            platform: "x".into(),
            chat_id: "c".into(),
            user_id: "u".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
        };
        let v = command_context(&incoming, "sk", "status", "/status");
        assert_eq!(
            object_keys(&v),
            vec![
                "chat_id",
                "command",
                "platform",
                "session_id",
                "text",
                "user_id"
            ]
        );
    }
}
