//! Event bridge -- reads StreamEvents from mpsc and writes NDJSON session/update notifications.

use crate::executor::StreamEvent;
use crate::ndjson::NdjsonWriter;
use serde_json::Value;
use tokio::io::AsyncWrite;

/// Summary returned by [`bridge_events`] after draining the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeResult {
    /// Total number of events received from the channel.
    pub total: usize,
    /// Number of events that could not be written (client disconnected).
    pub dropped: usize,
}

impl BridgeResult {
    /// Returns `true` if all events were written successfully.
    pub fn all_written(&self) -> bool {
        self.dropped == 0
    }
}

/// Bridge streaming events from executor to the IPC writer.
///
/// Reads from the mpsc receiver and formats each event as an ACP
/// session/update JSON-RPC notification, writing it immediately.
/// On write failure the bridge continues draining the channel (so the
/// executor never blocks on a full buffer) and returns a summary of
/// how many events were dropped.
pub async fn bridge_events<W: AsyncWrite + Unpin>(
    mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
    writer: &mut NdjsonWriter<W>,
    session_id: &str,
) -> BridgeResult {
    let mut total = 0usize;
    let mut dropped = 0usize;

    while let Some(event) = rx.recv().await {
        total += 1;
        let notification = format_session_update(session_id, &event);
        if let Err(e) = writer.write_json(&notification).await {
            if dropped == 0 {
                tracing::warn!(session_id, error = %e, "event bridge write error, draining remaining events");
            }
            dropped += 1;
            continue;
        }
        tracing::trace!(session_id, "bridged stream event");
    }

    if dropped > 0 {
        tracing::debug!(
            session_id,
            total,
            dropped,
            "drained events after write failure"
        );
    }

    BridgeResult { total, dropped }
}

/// Format a StreamEvent as an ACP session/update notification.
fn format_session_update(session_id: &str, event: &StreamEvent) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": event
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::StreamContent;

    #[test]
    fn test_format_agent_message_chunk() {
        let event = StreamEvent::AgentMessageChunk {
            content: StreamContent::Text {
                text: "Hello".to_string(),
            },
        };
        let notification = format_session_update("sess-1", &event);

        assert_eq!(notification["jsonrpc"], "2.0");
        assert_eq!(notification["method"], "session/update");
        assert_eq!(notification["params"]["sessionId"], "sess-1");

        let update = &notification["params"]["update"];
        assert_eq!(update["sessionUpdate"], "agent_message_chunk");
        assert_eq!(update["content"]["type"], "text");
        assert_eq!(update["content"]["text"], "Hello");
    }

    #[test]
    fn test_format_tool_call() {
        let event = StreamEvent::ToolCall {
            tool_call_id: "tc-1".to_string(),
            title: "read_file".to_string(),
            kind: "read".to_string(),
            raw_input: Some(serde_json::json!({"path": "/tmp/test.txt"})),
            status: "pending".to_string(),
        };
        let notification = format_session_update("sess-1", &event);

        let update = &notification["params"]["update"];
        assert_eq!(update["sessionUpdate"], "tool_call");
        assert_eq!(update["tool_call_id"], "tc-1");
        assert_eq!(update["title"], "read_file");
        assert_eq!(update["status"], "pending");
    }

    #[test]
    fn test_bridge_result_helpers() {
        let ok = BridgeResult {
            total: 5,
            dropped: 0,
        };
        assert!(ok.all_written());

        let partial = BridgeResult {
            total: 8,
            dropped: 3,
        };
        assert!(!partial.all_written());
        assert_eq!(partial.total, 8);
        assert_eq!(partial.dropped, 3);
    }
}
