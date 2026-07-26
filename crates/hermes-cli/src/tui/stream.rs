// ---------------------------------------------------------------------------
// Streaming support (Requirement 9.5)
// ---------------------------------------------------------------------------

fn bound_stream_text(text: String, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut bounded: String = chars.by_ref().take(max_chars.max(1)).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

/// A bounded handle for render-only stream events.
#[derive(Clone)]
pub struct StreamHandle {
    stream_sender: mpsc::Sender<Event>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    break_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    done_completed: tokio::sync::watch::Sender<u64>,
}

impl StreamHandle {
    pub fn new(stream_sender: mpsc::Sender<Event>) -> Self {
        let (done_completed, _) = tokio::sync::watch::channel(0);
        Self {
            stream_sender,
            dropped: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            break_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            done_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            done_completed,
        }
    }

    fn send_stream_event(&self, event: Event) -> bool {
        if self.stream_sender.try_send(event).is_err() {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    pub fn send_render_delta(&self, delta: hermes_agent::AgentRenderDelta) {
        const MAX_RENDER_DELTA_BYTES: usize = 4 * 1024;
        if delta.break_before {
            self.break_pending
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let text = match delta.text {
            Some(text) if !text.is_empty() => text,
            _ => {
                if delta.mute_post_response.is_some() {
                    let _ = self.send_stream_event(Event::StreamRender(
                        hermes_agent::AgentRenderDelta {
                            text: None,
                            mute_post_response: delta.mute_post_response,
                            break_before: false,
                        },
                    ));
                }
                return;
            }
        };
        let mut chunk = String::with_capacity(MAX_RENDER_DELTA_BYTES);
        for character in text.chars() {
            if !chunk.is_empty()
                && chunk.len().saturating_add(character.len_utf8()) > MAX_RENDER_DELTA_BYTES
            {
                self.send_render_text_chunk(
                    std::mem::take(&mut chunk),
                    delta.mute_post_response,
                );
                chunk = String::with_capacity(MAX_RENDER_DELTA_BYTES);
            }
            chunk.push(character);
        }
        if !chunk.is_empty() {
            self.send_render_text_chunk(chunk, delta.mute_post_response);
        }
    }

    fn send_render_text_chunk(&self, text: String, mute_post_response: Option<bool>) {
        let break_before = self
            .break_pending
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        if !self.send_stream_event(Event::StreamRender(hermes_agent::AgentRenderDelta {
            text: Some(text),
            mute_post_response,
            break_before,
        })) && break_before
        {
            self.break_pending
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    pub fn send_ui_event(&self, event: StreamUiEvent) {
        let event = match event {
            StreamUiEvent::Thinking(text) => StreamUiEvent::Thinking(bound_stream_text(text, 1_024)),
            StreamUiEvent::ToolStarted { name, args_preview } => StreamUiEvent::ToolStarted {
                name: bound_stream_text(name, 64),
                args_preview: bound_stream_text(args_preview, 256),
            },
            StreamUiEvent::ToolCompleted {
                name,
                result_preview,
            } => StreamUiEvent::ToolCompleted {
                name: bound_stream_text(name, 64),
                result_preview: bound_stream_text(result_preview, 512),
            },
            StreamUiEvent::Status { kind, message } => StreamUiEvent::Status {
                kind: bound_stream_text(kind, 64),
                message: bound_stream_text(message, 1_024),
            },
            StreamUiEvent::Phase {
                phase,
                label,
                progress_pct,
            } => StreamUiEvent::Phase {
                phase: bound_stream_text(phase, 64),
                label: bound_stream_text(label, 256),
                progress_pct: progress_pct.min(100),
            },
            StreamUiEvent::Lifecycle(message) => {
                StreamUiEvent::Lifecycle(bound_stream_text(message, 512))
            }
            StreamUiEvent::MoaReference {
                index,
                count,
                label,
                text,
            } => StreamUiEvent::MoaReference {
                index,
                count,
                label: bound_stream_text(label, 128),
                text: bound_stream_text(text, 1_024),
            },
        };
        let _ = self.send_stream_event(Event::StreamUi(event));
    }

    pub fn send_usage(&self, usage: hermes_core::UsageStats) {
        self.send_stream_event(Event::StreamUsage(usage));
    }

    pub fn record_upstream_drops(&self, count: u64) {
        self.dropped
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    }

    /// Send a streaming text delta to the TUI.
    pub fn send_delta(&self, text: &str) {
        self.send_render_delta(hermes_agent::AgentRenderDelta {
            text: Some(text.to_string()),
            mute_post_response: None,
            break_before: false,
        });
    }

    /// Schedule a reliable completion marker without blocking the UI producer.
    pub fn send_done(&self) {
        let generation = self
            .done_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .saturating_add(1);
        let dropped = self
            .dropped
            .swap(0, std::sync::atomic::Ordering::AcqRel);
        let stream_sender = self.stream_sender.clone();
        let done_completed = self.done_completed.clone();
        tokio::spawn(async move {
            if generation > 1 {
                let mut prior = done_completed.subscribe();
                while *prior.borrow_and_update() < generation - 1 {
                    if prior.changed().await.is_err() {
                        return;
                    }
                }
            }
            if dropped > 0 {
                let _ = stream_sender.send(Event::StreamOverflow(dropped)).await;
            }
            let _ = stream_sender.send(Event::AgentDone).await;
            done_completed.send_replace(generation);
        });
    }

    /// Wait until the latest scheduled completion marker has entered the lane.
    pub async fn wait_for_done(&self) {
        let target = self
            .done_generation
            .load(std::sync::atomic::Ordering::Acquire);
        if target == 0 {
            return;
        }
        let mut completed = self.done_completed.subscribe();
        while *completed.borrow_and_update() < target {
            if completed.changed().await.is_err() {
                return;
            }
        }
    }
}

