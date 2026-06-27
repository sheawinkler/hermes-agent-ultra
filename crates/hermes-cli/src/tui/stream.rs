// ---------------------------------------------------------------------------
// Streaming support (Requirement 9.5)
// ---------------------------------------------------------------------------

/// A handle for sending streaming deltas to the TUI.
///
/// Clone this and pass it to the agent loop's streaming callback.
/// The TUI will accumulate deltas and display them in real time.
#[derive(Clone)]
pub struct StreamHandle {
    sender: mpsc::UnboundedSender<Event>,
}

impl StreamHandle {
    /// Send a streaming text delta to the TUI.
    pub fn send_delta(&self, text: &str) {
        let _ = self.sender.send(Event::StreamDelta(text.to_string()));
    }

    /// Send a full streaming chunk to the TUI event loop.
    pub fn send_chunk(&self, chunk: StreamChunk) {
        let _ = self.sender.send(Event::StreamChunk(chunk));
    }

    /// Signal that the agent has finished.
    pub fn send_done(&self) {
        let _ = self.sender.send(Event::AgentDone);
    }
}

impl From<mpsc::UnboundedSender<Event>> for StreamHandle {
    fn from(sender: mpsc::UnboundedSender<Event>) -> Self {
        Self { sender }
    }
}

