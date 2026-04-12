//! Streaming output management for progressive message updates.
//!
//! Provides two layers:
//!
//! 1. **`StreamManager`** — a shared registry of active `StreamHandle`s, keyed
//!    by stream ID, for managing multiple concurrent streams.
//!
//! 2. **`StreamConsumer`** — a single-stream lifecycle manager modelled after
//!    the Python `GatewayStreamConsumer`.  It accumulates deltas, enforces
//!    rate-limited edit intervals, handles flood-control backoff, cursor
//!    animation, segment breaks (tool boundaries), and fallback delivery.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::Instant;

// ---------------------------------------------------------------------------
// StreamSegmentEvent
// ---------------------------------------------------------------------------

/// Events that flow through the stream pipeline.
///
/// Maps to the Python sentinels `_DONE`, `_NEW_SEGMENT`, `_COMMENTARY`, and
/// plain string deltas.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamSegmentEvent {
    /// A chunk of streamed text.
    Delta(String),
    /// Tool boundary — finalize the current message and start a fresh one so
    /// subsequent text appears below any tool-progress messages.
    SegmentBreak,
    /// A completed interim commentary message (e.g. "I'll inspect the repo
    /// first.") emitted between tool iterations.
    Commentary(String),
    /// The stream is complete.
    Done,
}

// ---------------------------------------------------------------------------
// StreamConfig
// ---------------------------------------------------------------------------

/// Configuration for streaming behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    /// Interval in milliseconds between progressive edit operations.
    #[serde(default = "default_edit_interval_ms")]
    pub edit_interval_ms: u64,

    /// Number of buffered characters before forcing a flush.
    #[serde(default = "default_buffer_threshold")]
    pub buffer_threshold: usize,

    /// Maximum length of a single message before splitting.
    #[serde(default = "default_max_message_length")]
    pub max_message_length: usize,

    /// Cursor glyph appended to the message while streaming is in progress.
    /// Removed on the final flush.
    #[serde(default = "default_cursor")]
    pub cursor: String,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            edit_interval_ms: default_edit_interval_ms(),
            buffer_threshold: default_buffer_threshold(),
            max_message_length: default_max_message_length(),
            cursor: default_cursor(),
        }
    }
}

fn default_edit_interval_ms() -> u64 {
    300
}

fn default_buffer_threshold() -> usize {
    50
}

fn default_max_message_length() -> usize {
    4096
}

fn default_cursor() -> String {
    " ▉".to_string()
}

// ---------------------------------------------------------------------------
// StreamHandle
// ---------------------------------------------------------------------------

/// Handle to an active streaming session, tracking the platform, chat,
/// message ID, and accumulated content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamHandle {
    /// Unique identifier for this stream.
    pub id: String,

    /// Platform name (e.g., "telegram").
    pub platform: String,

    /// Chat/channel identifier.
    pub chat_id: String,

    /// The message ID on the platform (set after the first edit).
    pub message_id: Option<String>,

    /// Accumulated content so far.
    pub content: String,

    /// Characters since last edit (buffer counter).
    pub buffered_since_edit: usize,

    /// When this stream was created.
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// When the last edit was performed.
    pub last_edit_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Whether edit operations are still supported for this stream.
    #[serde(default = "default_true")]
    pub edit_supported: bool,

    /// Whether the final response should be sent via fallback (new message)
    /// because progressive edits stopped working mid-stream.
    #[serde(default)]
    pub fallback_final_send: bool,

    /// The visible text prefix already delivered to the user before edits
    /// stopped working.  Used by the fallback path to send only the unseen
    /// continuation.
    #[serde(default)]
    pub fallback_prefix: String,

    /// Consecutive flood-control edit failures.
    #[serde(default)]
    pub flood_strikes: u8,

    /// Maximum flood strikes before permanently disabling edits.
    #[serde(default = "default_max_flood_strikes")]
    pub max_flood_strikes: u8,

    /// Adaptive edit interval (milliseconds) — doubles on each flood strike.
    #[serde(default = "default_edit_interval_ms")]
    pub current_edit_interval_ms: u64,

    /// Whether at least one message was successfully sent or edited.
    #[serde(default)]
    pub already_sent: bool,

    /// Whether the definitive final response has been delivered.
    #[serde(default)]
    pub final_response_sent: bool,

    /// Text of the last successfully sent/edited message.  Used to skip
    /// redundant edits when content hasn't changed.
    #[serde(default)]
    pub last_sent_text: String,
}

fn default_true() -> bool {
    true
}

fn default_max_flood_strikes() -> u8 {
    3
}

impl StreamHandle {
    /// Create a new stream handle.
    pub fn new(platform: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            platform: platform.into(),
            chat_id: chat_id.into(),
            message_id: None,
            content: String::new(),
            buffered_since_edit: 0,
            created_at: chrono::Utc::now(),
            last_edit_at: None,
            edit_supported: true,
            fallback_final_send: false,
            fallback_prefix: String::new(),
            flood_strikes: 0,
            max_flood_strikes: default_max_flood_strikes(),
            current_edit_interval_ms: default_edit_interval_ms(),
            already_sent: false,
            final_response_sent: false,
            last_sent_text: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// StreamManager
// ---------------------------------------------------------------------------

/// Manages streaming output sessions with progressive editing.
pub struct StreamManager {
    /// Active stream handles, keyed by stream ID.
    streams: RwLock<HashMap<String, StreamHandle>>,

    /// Configuration for edit intervals and thresholds.
    config: StreamConfig,
}

impl StreamManager {
    /// Create a new `StreamManager` with the given configuration.
    pub fn new(config: StreamConfig) -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Create a `StreamManager` with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(StreamConfig::default())
    }

    /// Start a new streaming session. Returns a `StreamHandle` for tracking.
    pub async fn start_stream(&self, platform: &str, chat_id: &str) -> StreamHandle {
        let handle = StreamHandle::new(platform, chat_id);
        let id = handle.id.clone();
        self.streams.write().await.insert(id, handle.clone());
        handle
    }

    /// Update a streaming session with new content.
    ///
    /// Returns `true` if the content should be flushed (edited on platform)
    /// based on the buffer threshold or edit interval.
    pub async fn update_stream(&self, stream_id: &str, new_content: &str) -> Option<bool> {
        let mut streams = self.streams.write().await;
        let handle = streams.get_mut(stream_id)?;

        handle.content.push_str(new_content);
        handle.buffered_since_edit += new_content.len();

        let now = chrono::Utc::now();
        let should_flush = if let Some(last_edit) = handle.last_edit_at {
            let elapsed_ms = (now - last_edit).num_milliseconds() as u64;
            elapsed_ms >= self.config.edit_interval_ms
                || handle.buffered_since_edit >= self.config.buffer_threshold
        } else {
            handle.buffered_since_edit >= self.config.buffer_threshold
        };

        if should_flush {
            handle.buffered_since_edit = 0;
            handle.last_edit_at = Some(now);
        }

        Some(should_flush)
    }

    /// Mark a streaming session as finished. Returns the final content.
    ///
    /// Removes the stream handle from the manager.
    pub async fn finish_stream(&self, stream_id: &str) -> Option<String> {
        let mut streams = self.streams.write().await;
        streams.remove(stream_id).map(|h| h.content)
    }

    /// Get the current content of a stream without finishing it.
    pub async fn get_stream_content(&self, stream_id: &str) -> Option<String> {
        let streams = self.streams.read().await;
        streams.get(stream_id).map(|h| h.content.clone())
    }

    /// Set the message ID for a stream (after the first platform edit).
    pub async fn set_message_id(&self, stream_id: &str, message_id: &str) {
        let mut streams = self.streams.write().await;
        if let Some(handle) = streams.get_mut(stream_id) {
            handle.message_id = Some(message_id.to_string());
        }
    }

    /// Check if the content exceeds the maximum message length.
    pub fn should_split(&self, content: &str) -> bool {
        content.len() > self.config.max_message_length
    }

    /// Split content at the maximum message length boundary.
    /// Returns a vector of string chunks.
    pub fn split_content(&self, content: &str) -> Vec<String> {
        let max_len = self.config.max_message_length;
        if content.len() <= max_len {
            return vec![content.to_string()];
        }

        let mut chunks = Vec::new();
        let mut start = 0;
        while start < content.len() {
            let end = std::cmp::min(start + max_len, content.len());
            let mut break_at = end;
            if end < content.len() {
                if let Some(nl_pos) = content[start..end].rfind('\n') {
                    break_at = start + nl_pos + 1;
                }
            }
            chunks.push(content[start..break_at].to_string());
            start = break_at;
        }
        chunks
    }

    /// Get the number of active streams.
    pub async fn active_stream_count(&self) -> usize {
        self.streams.read().await.len()
    }

    /// Get the config reference.
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// StreamConsumer
// ---------------------------------------------------------------------------

/// Single-stream lifecycle manager — the Rust analogue of
/// `GatewayStreamConsumer` in Python.
///
/// Accumulates deltas, enforces rate-limited edit intervals, handles
/// flood-control backoff, cursor animation, segment breaks (tool boundaries),
/// and fallback delivery when progressive edits stop working.
///
/// This struct is *transport-agnostic*: it decides **what** to display and
/// **when** to flush, but the actual send/edit calls remain the caller's
/// responsibility (via the returned decisions from `should_flush`,
/// `get_display_text`, `needs_new_message`, etc.).
#[derive(Debug)]
pub struct StreamConsumer {
    config: StreamConfig,
    chat_id: String,
    platform: String,
    message_id: Option<String>,
    accumulated: String,
    already_sent: bool,
    edit_supported: bool,
    last_edit_time: Option<Instant>,
    last_sent_text: String,
    fallback_final_send: bool,
    fallback_prefix: String,
    flood_strikes: u8,
    max_flood_strikes: u8,
    current_edit_interval_ms: u64,
    final_response_sent: bool,
}

impl StreamConsumer {
    /// Create a new consumer for a single stream.
    pub fn new(platform: &str, chat_id: &str, config: StreamConfig) -> Self {
        let interval = config.edit_interval_ms;
        Self {
            config,
            chat_id: chat_id.to_string(),
            platform: platform.to_string(),
            message_id: None,
            accumulated: String::new(),
            already_sent: false,
            edit_supported: true,
            last_edit_time: None,
            last_sent_text: String::new(),
            fallback_final_send: false,
            fallback_prefix: String::new(),
            flood_strikes: 0,
            max_flood_strikes: default_max_flood_strikes(),
            current_edit_interval_ms: interval,
            final_response_sent: false,
        }
    }

    // -- Accessors ----------------------------------------------------------

    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    pub fn accumulated(&self) -> &str {
        &self.accumulated
    }

    pub fn already_sent(&self) -> bool {
        self.already_sent
    }

    pub fn final_response_sent(&self) -> bool {
        self.final_response_sent
    }

    pub fn edit_supported(&self) -> bool {
        self.edit_supported
    }

    pub fn fallback_final_send(&self) -> bool {
        self.fallback_final_send
    }

    pub fn flood_strikes(&self) -> u8 {
        self.flood_strikes
    }

    pub fn current_edit_interval_ms(&self) -> u64 {
        self.current_edit_interval_ms
    }

    pub fn config(&self) -> &StreamConfig {
        &self.config
    }

    // -- Delta / segment lifecycle ------------------------------------------

    /// Accumulate a text delta.
    pub fn on_delta(&mut self, text: &str) {
        if !text.is_empty() {
            self.accumulated.push_str(text);
        }
    }

    /// Finalize the current segment.  The caller should deliver the current
    /// `accumulated` text as a final (cursor-less) edit, then call
    /// `reset_segment` before feeding subsequent deltas.
    pub fn on_segment_break(&mut self) {
        // Nothing to mutate here — the caller inspects `accumulated` and
        // decides how to flush.  This is intentionally a no-op hook so that
        // higher-level orchestrators can pattern-match on it.
    }

    /// Signal that the stream is done.
    pub fn mark_done(&mut self) {
        if self.already_sent && !self.accumulated.is_empty() {
            self.final_response_sent = true;
        }
    }

    // -- Flush decision -----------------------------------------------------

    /// Should the caller flush an edit right now?
    ///
    /// Returns `true` when enough time has elapsed since the last edit *or*
    /// the buffer has grown past the threshold.
    pub fn should_flush(&self) -> bool {
        if self.accumulated.is_empty() {
            return false;
        }
        if !self.edit_supported {
            return false;
        }

        if self.accumulated.len() >= self.config.buffer_threshold {
            return true;
        }

        match self.last_edit_time {
            Some(last) => {
                let elapsed = last.elapsed().as_millis() as u64;
                elapsed >= self.current_edit_interval_ms
            }
            None => self.accumulated.len() >= self.config.buffer_threshold,
        }
    }

    // -- Display text -------------------------------------------------------

    /// Return the text to display.  Appends the cursor when `is_final` is
    /// false.
    pub fn get_display_text(&self, is_final: bool) -> String {
        if is_final || self.config.cursor.is_empty() {
            self.accumulated.clone()
        } else {
            format!("{}{}", self.accumulated, self.config.cursor)
        }
    }

    /// The visible prefix already delivered to the user — the last sent text
    /// with the cursor stripped.
    fn visible_prefix(&self) -> String {
        let prefix = &self.last_sent_text;
        if !self.config.cursor.is_empty() && prefix.ends_with(&self.config.cursor) {
            prefix[..prefix.len() - self.config.cursor.len()].to_string()
        } else {
            prefix.clone()
        }
    }

    /// Return only the portion of `final_text` the user has **not** already
    /// seen.  Used by the fallback path to avoid re-sending visible content.
    pub fn continuation_text(&self, final_text: &str) -> String {
        let prefix = if self.fallback_prefix.is_empty() {
            self.visible_prefix()
        } else {
            self.fallback_prefix.clone()
        };
        if !prefix.is_empty() && final_text.starts_with(&prefix) {
            final_text[prefix.len()..].trim_start().to_string()
        } else {
            final_text.to_string()
        }
    }

    // -- Edit result callbacks ----------------------------------------------

    /// Record a successful edit.  Resets flood strikes and updates tracking.
    pub fn mark_edit_success(&mut self, message_id: &str) {
        self.message_id = Some(message_id.to_string());
        self.already_sent = true;
        self.last_sent_text = self.get_display_text(false);
        self.last_edit_time = Some(Instant::now());
        self.flood_strikes = 0;
    }

    /// Record a failed edit.
    ///
    /// When `is_flood` is true, uses adaptive backoff: doubles the edit
    /// interval and increments the flood-strike counter.  After
    /// `max_flood_strikes` consecutive flood failures, permanently disables
    /// progressive edits and enters fallback mode.
    ///
    /// Non-flood failures immediately enter fallback mode.
    pub fn mark_edit_failed(&mut self, is_flood: bool) {
        if is_flood {
            self.flood_strikes += 1;
            self.current_edit_interval_ms = (self.current_edit_interval_ms * 2).min(10_000);

            if self.flood_strikes < self.max_flood_strikes {
                self.last_edit_time = Some(Instant::now());
                return;
            }
        }

        self.fallback_prefix = self.visible_prefix();
        self.fallback_final_send = true;
        self.edit_supported = false;
        self.already_sent = true;
    }

    // -- Segment management -------------------------------------------------

    /// Does the consumer need the caller to start a new message (i.e., there
    /// is no current `message_id` to edit)?
    pub fn needs_new_message(&self) -> bool {
        self.message_id.is_none()
    }

    /// Reset segment state after a segment break or commentary delivery.
    /// Clears `message_id`, `accumulated`, `last_sent_text`, and the
    /// fallback fields so the next delta starts a fresh message.
    pub fn reset_segment(&mut self) {
        self.message_id = None;
        self.accumulated.clear();
        self.last_sent_text.clear();
        self.fallback_final_send = false;
        self.fallback_prefix.clear();
    }

    /// Variant of `reset_segment` that preserves the `"__no_edit__"` sentinel
    /// — used on segment breaks when the platform never returned a real
    /// message ID.
    pub fn reset_segment_preserve_no_edit(&mut self) {
        if self.message_id.as_deref() == Some("__no_edit__") {
            return;
        }
        self.reset_segment();
    }

    /// Whether the last sent text matches `text`, meaning an edit would be
    /// redundant.
    pub fn is_redundant_edit(&self, text: &str) -> bool {
        !self.last_sent_text.is_empty() && self.last_sent_text == text
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- StreamConfig -------------------------------------------------------

    #[test]
    fn stream_config_default() {
        let config = StreamConfig::default();
        assert_eq!(config.edit_interval_ms, 300);
        assert_eq!(config.buffer_threshold, 50);
        assert_eq!(config.max_message_length, 4096);
        assert_eq!(config.cursor, " ▉");
    }

    // -- StreamSegmentEvent -------------------------------------------------

    #[test]
    fn segment_event_variants() {
        let delta = StreamSegmentEvent::Delta("hello".into());
        assert_eq!(delta, StreamSegmentEvent::Delta("hello".into()));
        assert_ne!(delta, StreamSegmentEvent::SegmentBreak);
        assert_ne!(delta, StreamSegmentEvent::Done);

        let commentary = StreamSegmentEvent::Commentary("thinking…".into());
        assert_eq!(
            commentary,
            StreamSegmentEvent::Commentary("thinking…".into()),
        );
    }

    // -- StreamHandle -------------------------------------------------------

    #[test]
    fn stream_handle_new() {
        let handle = StreamHandle::new("telegram", "chat123");
        assert_eq!(handle.platform, "telegram");
        assert_eq!(handle.chat_id, "chat123");
        assert!(handle.message_id.is_none());
        assert!(handle.content.is_empty());
        assert!(handle.edit_supported);
        assert!(!handle.already_sent);
        assert!(!handle.final_response_sent);
        assert!(!handle.fallback_final_send);
        assert_eq!(handle.flood_strikes, 0);
        assert_eq!(handle.max_flood_strikes, 3);
        assert_eq!(handle.current_edit_interval_ms, 300);
        assert!(handle.last_sent_text.is_empty());
    }

    // -- StreamManager ------------------------------------------------------

    #[tokio::test]
    async fn stream_manager_start_and_finish() {
        let manager = StreamManager::with_defaults();
        let handle = manager.start_stream("telegram", "chat1").await;
        let id = handle.id.clone();

        let content = manager.get_stream_content(&id).await;
        assert_eq!(content, Some(String::new()));

        let final_content = manager.finish_stream(&id).await;
        assert_eq!(final_content, Some(String::new()));

        assert!(manager.get_stream_content(&id).await.is_none());
    }

    #[tokio::test]
    async fn stream_manager_update_and_flush() {
        let manager = StreamManager::new(StreamConfig {
            edit_interval_ms: 0,
            buffer_threshold: 5,
            max_message_length: 4096,
            cursor: default_cursor(),
        });

        let handle = manager.start_stream("telegram", "chat1").await;
        let id = handle.id.clone();

        let should_flush = manager.update_stream(&id, "hi").await;
        assert!(should_flush.is_some());

        let should_flush = manager.update_stream(&id, "hello world!").await;
        assert_eq!(should_flush, Some(true));
    }

    #[test]
    fn stream_manager_split_content_short() {
        let config = StreamConfig {
            max_message_length: 100,
            ..Default::default()
        };
        let manager = StreamManager::new(config);
        let chunks = manager.split_content("short message");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn stream_manager_split_content_long() {
        let config = StreamConfig {
            max_message_length: 10,
            ..Default::default()
        };
        let manager = StreamManager::new(config);
        let long = "abcdefghij\nklmnopqrst";
        let chunks = manager.split_content(long);
        assert!(chunks.len() >= 2);
    }

    // -- StreamConsumer -----------------------------------------------------

    #[test]
    fn consumer_new_defaults() {
        let c = StreamConsumer::new("telegram", "chat1", StreamConfig::default());
        assert_eq!(c.platform(), "telegram");
        assert_eq!(c.chat_id(), "chat1");
        assert!(c.message_id().is_none());
        assert!(c.accumulated().is_empty());
        assert!(!c.already_sent());
        assert!(!c.final_response_sent());
        assert!(c.edit_supported());
        assert!(!c.fallback_final_send());
        assert_eq!(c.flood_strikes(), 0);
        assert_eq!(c.current_edit_interval_ms(), 300);
    }

    #[test]
    fn consumer_on_delta_accumulates() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("Hello");
        c.on_delta(", world!");
        assert_eq!(c.accumulated(), "Hello, world!");
    }

    #[test]
    fn consumer_empty_delta_ignored() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("");
        assert!(c.accumulated().is_empty());
    }

    #[test]
    fn consumer_display_text_with_cursor() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("streaming");
        assert_eq!(c.get_display_text(false), "streaming ▉");
        assert_eq!(c.get_display_text(true), "streaming");
    }

    #[test]
    fn consumer_display_text_no_cursor() {
        let config = StreamConfig {
            cursor: String::new(),
            ..Default::default()
        };
        let mut c = StreamConsumer::new("tg", "c1", config);
        c.on_delta("streaming");
        assert_eq!(c.get_display_text(false), "streaming");
        assert_eq!(c.get_display_text(true), "streaming");
    }

    #[test]
    fn consumer_mark_edit_success() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("hello");
        c.mark_edit_success("msg_42");

        assert_eq!(c.message_id(), Some("msg_42"));
        assert!(c.already_sent());
        assert_eq!(c.flood_strikes(), 0);
        assert_eq!(c.last_sent_text, "hello ▉");
    }

    #[test]
    fn consumer_flood_backoff() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("text");
        c.mark_edit_success("msg_1");

        // First flood strike — doubles interval, does NOT disable edits.
        c.mark_edit_failed(true);
        assert_eq!(c.flood_strikes(), 1);
        assert_eq!(c.current_edit_interval_ms(), 600);
        assert!(c.edit_supported());

        // Second flood strike
        c.mark_edit_failed(true);
        assert_eq!(c.flood_strikes(), 2);
        assert_eq!(c.current_edit_interval_ms(), 1200);
        assert!(c.edit_supported());

        // Third strike — disables edits, enters fallback
        c.mark_edit_failed(true);
        assert_eq!(c.flood_strikes(), 3);
        assert!(!c.edit_supported());
        assert!(c.fallback_final_send());
    }

    #[test]
    fn consumer_non_flood_failure_immediate_fallback() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("text");
        c.mark_edit_success("msg_1");

        c.mark_edit_failed(false);
        assert!(!c.edit_supported());
        assert!(c.fallback_final_send());
        assert!(c.already_sent());
    }

    #[test]
    fn consumer_continuation_text() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("Hello world, ");
        c.mark_edit_success("msg_1");

        // Simulate fallback — the visible prefix is "Hello world, " (cursor stripped).
        c.mark_edit_failed(false);

        let full = "Hello world, this is the rest";
        let cont = c.continuation_text(full);
        assert_eq!(cont, "this is the rest");
    }

    #[test]
    fn consumer_continuation_no_prefix() {
        let c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        let cont = c.continuation_text("brand new text");
        assert_eq!(cont, "brand new text");
    }

    #[test]
    fn consumer_needs_new_message() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        assert!(c.needs_new_message());

        c.mark_edit_success("msg_1");
        assert!(!c.needs_new_message());
    }

    #[test]
    fn consumer_reset_segment() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("data");
        c.mark_edit_success("msg_1");
        c.fallback_final_send = true;
        c.fallback_prefix = "prefix".to_string();

        c.reset_segment();

        assert!(c.message_id().is_none());
        assert!(c.accumulated().is_empty());
        assert!(c.last_sent_text.is_empty());
        assert!(!c.fallback_final_send());
        assert!(c.fallback_prefix.is_empty());
    }

    #[test]
    fn consumer_reset_segment_preserve_no_edit() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.message_id = Some("__no_edit__".to_string());
        c.on_delta("data");

        c.reset_segment_preserve_no_edit();
        assert_eq!(c.message_id(), Some("__no_edit__"));
        assert!(!c.accumulated().is_empty());
    }

    #[test]
    fn consumer_is_redundant_edit() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        assert!(!c.is_redundant_edit("anything"));

        c.on_delta("hello");
        c.mark_edit_success("msg_1");
        assert!(c.is_redundant_edit("hello ▉"));
        assert!(!c.is_redundant_edit("hello world ▉"));
    }

    #[test]
    fn consumer_should_flush_empty() {
        let c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        assert!(!c.should_flush());
    }

    #[test]
    fn consumer_should_flush_buffer_threshold() {
        let config = StreamConfig {
            buffer_threshold: 5,
            ..Default::default()
        };
        let mut c = StreamConsumer::new("tg", "c1", config);
        c.on_delta("hi");
        // 2 chars < threshold 5 and no prior edit → no flush
        assert!(!c.should_flush());

        c.on_delta("hello");
        // 7 chars ≥ threshold 5 → flush
        assert!(c.should_flush());
    }

    #[test]
    fn consumer_should_flush_not_when_edits_disabled() {
        let config = StreamConfig {
            buffer_threshold: 2,
            ..Default::default()
        };
        let mut c = StreamConsumer::new("tg", "c1", config);
        c.on_delta("lots of text");
        c.edit_supported = false;
        assert!(!c.should_flush());
    }

    #[test]
    fn consumer_mark_done() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.on_delta("response");
        c.already_sent = true;
        c.mark_done();
        assert!(c.final_response_sent());
    }

    #[test]
    fn consumer_mark_done_no_content() {
        let mut c = StreamConsumer::new("tg", "c1", StreamConfig::default());
        c.already_sent = true;
        c.mark_done();
        assert!(!c.final_response_sent());
    }
}
