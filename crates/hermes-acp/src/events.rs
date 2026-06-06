//! ACP event types for bridging agent progress to the ACP client.
//!
//! Provides callback factories that bridge agent events (tool progress,
//! thinking, step completion, message streaming) to ACP session update
//! notifications.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::protocol::AvailableCommand;
use crate::tools::{format_tool_result, tool_completion_status, tool_start_metadata};

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Types of events that can be emitted during an ACP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpEventKind {
    /// Agent is thinking / reasoning.
    Thinking,
    /// A tool call has started.
    ToolCallStart,
    /// A tool call has completed.
    ToolCallComplete,
    /// Streaming message text from the agent.
    MessageDelta,
    /// Complete agent message.
    MessageComplete,
    /// Replayed or externally supplied user message chunk.
    UserMessageChunk,
    /// Replayed or externally supplied assistant message chunk.
    AgentMessageChunk,
    /// Replayed or externally supplied assistant thought chunk.
    AgentThoughtChunk,
    /// Step completed (one LLM turn with optional tool calls).
    StepComplete,
    /// Session-level progress update.
    Progress,
    /// Available slash commands changed.
    AvailableCommandsUpdate,
    /// Session metadata changed.
    SessionInfoUpdate,
    /// An error occurred.
    Error,
}

/// A single ACP event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpEvent {
    pub kind: AcpEventKind,
    pub session_id: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_call_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(
        rename = "sessionUpdate",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_update: Option<String>,
    #[serde(
        rename = "availableCommands",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub available_commands: Option<Vec<AvailableCommand>>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl AcpEvent {
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    pub fn thinking(session_id: &str, text: &str) -> Self {
        Self {
            kind: AcpEventKind::Thinking,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            text: Some(text.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            api_call_count: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn tool_call_start(
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> Self {
        let metadata = tool_start_metadata(tool_name, arguments.as_ref());
        Self {
            kind: AcpEventKind::ToolCallStart,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            tool_kind: Some(metadata.kind.to_string()),
            title: Some(metadata.title),
            arguments,
            text: None,
            result: None,
            status: None,
            content: None,
            api_call_count: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn tool_call_complete(
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        result: Option<String>,
    ) -> Self {
        let status = tool_completion_status(tool_name, result.as_deref());
        let result = format_tool_result(tool_name, result.as_deref());
        Self {
            kind: AcpEventKind::ToolCallComplete,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            tool_kind: Some(crate::tools::tool_kind(tool_name).to_string()),
            title: Some(tool_start_metadata(tool_name, None).title),
            result,
            text: None,
            arguments: None,
            status: Some(status.to_string()),
            content: None,
            api_call_count: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn message_delta(session_id: &str, text: &str) -> Self {
        Self {
            kind: AcpEventKind::MessageDelta,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            text: Some(text.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            api_call_count: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn message_complete(session_id: &str, text: &str) -> Self {
        Self {
            kind: AcpEventKind::MessageComplete,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            text: Some(text.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            api_call_count: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn user_message_chunk(session_id: &str, text: &str) -> Self {
        Self::history_text_chunk(
            AcpEventKind::UserMessageChunk,
            "user_message_chunk",
            session_id,
            text,
        )
    }

    pub fn agent_message_chunk(session_id: &str, text: &str) -> Self {
        Self::history_text_chunk(
            AcpEventKind::AgentMessageChunk,
            "agent_message_chunk",
            session_id,
            text,
        )
    }

    pub fn agent_thought_chunk(session_id: &str, text: &str) -> Self {
        Self::history_text_chunk(
            AcpEventKind::AgentThoughtChunk,
            "agent_thought_chunk",
            session_id,
            text,
        )
    }

    fn history_text_chunk(
        kind: AcpEventKind,
        session_update: &str,
        session_id: &str,
        text: &str,
    ) -> Self {
        Self {
            kind,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            session_update: Some(session_update.to_string()),
            content: Some(json!({"type": "text", "text": text})),
            text: Some(text.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            api_call_count: None,
            error: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn step_complete(session_id: &str, api_call_count: u32) -> Self {
        Self {
            kind: AcpEventKind::StepComplete,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            api_call_count: Some(api_call_count),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            text: None,
            error: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn error(session_id: &str, error: &str) -> Self {
        Self {
            kind: AcpEventKind::Error,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            error: Some(error.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            text: None,
            api_call_count: None,
            session_update: None,
            available_commands: None,
            updated_at: None,
        }
    }

    pub fn available_commands_update(session_id: &str, commands: Vec<AvailableCommand>) -> Self {
        Self {
            kind: AcpEventKind::AvailableCommandsUpdate,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            session_update: Some("available_commands_update".to_string()),
            available_commands: Some(commands),
            updated_at: None,
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            title: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            text: None,
            api_call_count: None,
            error: None,
        }
    }

    pub fn session_info_update(
        session_id: &str,
        title: Option<String>,
        updated_at: String,
    ) -> Self {
        Self {
            kind: AcpEventKind::SessionInfoUpdate,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            session_update: Some("session_info_update".to_string()),
            title,
            updated_at: Some(updated_at),
            tool_call_id: None,
            tool_name: None,
            tool_kind: None,
            arguments: None,
            result: None,
            status: None,
            content: None,
            text: None,
            api_call_count: None,
            error: None,
            available_commands: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolCallIdTracker
// ---------------------------------------------------------------------------

/// Tracks tool call IDs per tool name using a FIFO queue.
#[derive(Debug, Default)]
pub struct ToolCallIdTracker {
    queues: HashMap<String, VecDeque<String>>,
    counter: u64,
}

impl ToolCallIdTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate a new tool call ID and enqueue it for the given tool name.
    pub fn push(&mut self, tool_name: &str) -> String {
        self.counter += 1;
        let id = format!("tc_{:08x}", self.counter);
        self.queues
            .entry(tool_name.to_string())
            .or_default()
            .push_back(id.clone());
        id
    }

    /// Pop the oldest tool call ID for the given tool name.
    pub fn pop(&mut self, tool_name: &str) -> Option<String> {
        let queue = self.queues.get_mut(tool_name)?;
        let id = queue.pop_front();
        if queue.is_empty() {
            self.queues.remove(tool_name);
        }
        id
    }
}

// ---------------------------------------------------------------------------
// EventSink
// ---------------------------------------------------------------------------

/// Thread-safe event sink that collects ACP events for polling.
pub struct EventSink {
    events: Mutex<VecDeque<AcpEvent>>,
    max_events: usize,
}

impl EventSink {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: Mutex::new(VecDeque::new()),
            max_events,
        }
    }

    pub fn push(&self, event: AcpEvent) {
        let mut events = self.events.lock().unwrap();
        events.push_back(event);
        while events.len() > self.max_events {
            events.pop_front();
        }
    }

    pub fn drain_all(&self) -> Vec<AcpEvent> {
        let mut events = self.events.lock().unwrap();
        events.drain(..).collect()
    }

    pub fn drain_for_session(&self, session_id: &str) -> Vec<AcpEvent> {
        let mut events = self.events.lock().unwrap();
        let mut remaining = VecDeque::new();
        let mut result = Vec::new();
        for event in events.drain(..) {
            if event.session_id == session_id {
                result.push(event);
            } else {
                remaining.push_back(event);
            }
        }
        *events = remaining;
        result
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().unwrap().is_empty()
    }
}

impl Default for EventSink {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_call_id_tracker() {
        let mut tracker = ToolCallIdTracker::new();
        let id1 = tracker.push("read_file");
        let id2 = tracker.push("read_file");
        let id3 = tracker.push("write_file");

        assert_eq!(tracker.pop("read_file"), Some(id1));
        assert_eq!(tracker.pop("read_file"), Some(id2));
        assert_eq!(tracker.pop("read_file"), None);
        assert_eq!(tracker.pop("write_file"), Some(id3));
    }

    #[test]
    fn test_event_sink() {
        let sink = EventSink::new(5);
        sink.push(AcpEvent::thinking("s1", "hmm"));
        sink.push(AcpEvent::thinking("s2", "ok"));
        sink.push(AcpEvent::thinking("s1", "done"));

        let s1_events = sink.drain_for_session("s1");
        assert_eq!(s1_events.len(), 2);
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn test_event_sink_max_capacity() {
        let sink = EventSink::new(3);
        for i in 0..5 {
            sink.push(AcpEvent::thinking("s1", &format!("msg{}", i)));
        }
        assert_eq!(sink.len(), 3);
    }

    #[test]
    fn test_acp_event_kinds() {
        let e = AcpEvent::tool_call_start(
            "s1",
            "tc1",
            "read_file",
            Some(serde_json::json!({"path": "/tmp/a.txt"})),
        );
        assert_eq!(e.kind, AcpEventKind::ToolCallStart);
        assert_eq!(e.tool_name.as_deref(), Some("read_file"));
        assert_eq!(e.tool_kind.as_deref(), Some("read"));
        assert_eq!(e.title.as_deref(), Some("read: /tmp/a.txt"));

        let e2 = AcpEvent::tool_call_complete(
            "s1",
            "tc1",
            "read_file",
            Some(r#"{"error":"missing"}"#.into()),
        );
        assert_eq!(e2.result.as_deref(), Some("Read failed: missing"));
        assert_eq!(e2.tool_kind.as_deref(), Some("read"));
        assert_eq!(e2.status.as_deref(), Some("failed"));

        let e3 = AcpEvent::tool_call_complete(
            "s1",
            "tc2",
            "terminal",
            Some("Error: pytest collected 0 items".into()),
        );
        assert_eq!(
            e3.result.as_deref(),
            Some("Error: pytest collected 0 items")
        );
        assert_eq!(e3.status.as_deref(), Some("completed"));
    }
}
