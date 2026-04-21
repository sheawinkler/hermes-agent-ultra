//! ACP event types for bridging agent progress to the ACP client.
//!
//! Mirrors the Python `acp_adapter/events.py` — provides callback factories
//! that bridge AIAgent events (tool progress, thinking, step completion,
//! message streaming) to ACP session update notifications.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Step completed (one LLM turn with optional tool calls).
    StepComplete,
    /// Session-level progress update.
    Progress,
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
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_call_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
            arguments: None,
            result: None,
            api_call_count: None,
            error: None,
        }
    }

    pub fn tool_call_start(
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> Self {
        Self {
            kind: AcpEventKind::ToolCallStart,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            arguments,
            text: None,
            result: None,
            api_call_count: None,
            error: None,
        }
    }

    pub fn tool_call_complete(
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        result: Option<String>,
    ) -> Self {
        Self {
            kind: AcpEventKind::ToolCallComplete,
            session_id: session_id.to_string(),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            result,
            text: None,
            arguments: None,
            api_call_count: None,
            error: None,
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
            arguments: None,
            result: None,
            api_call_count: None,
            error: None,
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
            arguments: None,
            result: None,
            api_call_count: None,
            error: None,
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
            arguments: None,
            result: None,
            text: None,
            error: None,
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
            arguments: None,
            result: None,
            text: None,
            api_call_count: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolCallIdTracker
// ---------------------------------------------------------------------------

/// Tracks tool call IDs per tool name using a FIFO queue, matching the Python
/// `tool_call_ids: dict[str, Deque[str]]` pattern.
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
        let e = AcpEvent::tool_call_start("s1", "tc1", "read_file", None);
        assert_eq!(e.kind, AcpEventKind::ToolCallStart);
        assert_eq!(e.tool_name.as_deref(), Some("read_file"));

        let e2 = AcpEvent::tool_call_complete("s1", "tc1", "read_file", Some("ok".into()));
        assert_eq!(e2.result.as_deref(), Some("ok"));
    }
}
