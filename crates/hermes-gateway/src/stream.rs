//! Streaming output management for progressive message updates.
//!
//! The `StreamManager` handles streaming LLM output by progressively editing
//! messages on the target platform, with configurable edit intervals, buffer
//! thresholds, and maximum message lengths.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// StreamConfig
// ---------------------------------------------------------------------------

/// Configuration for the stream manager.
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
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            edit_interval_ms: default_edit_interval_ms(),
            buffer_threshold: default_buffer_threshold(),
            max_message_length: default_max_message_length(),
        }
    }
}

fn default_edit_interval_ms() -> u64 {
    1000
}

fn default_buffer_threshold() -> usize {
    50
}

fn default_max_message_length() -> usize {
    4096
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
            // First content: flush if we've reached the threshold
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
            // Try to break at a newline near the boundary
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_config_default() {
        let config = StreamConfig::default();
        assert_eq!(config.edit_interval_ms, 1000);
        assert_eq!(config.buffer_threshold, 50);
        assert_eq!(config.max_message_length, 4096);
    }

    #[test]
    fn stream_handle_new() {
        let handle = StreamHandle::new("telegram", "chat123");
        assert_eq!(handle.platform, "telegram");
        assert_eq!(handle.chat_id, "chat123");
        assert!(handle.message_id.is_none());
        assert!(handle.content.is_empty());
    }

    #[tokio::test]
    async fn stream_manager_start_and_finish() {
        let manager = StreamManager::with_defaults();
        let handle = manager.start_stream("telegram", "chat1").await;
        let id = handle.id.clone();

        // Content should be empty initially
        let content = manager.get_stream_content(&id).await;
        assert_eq!(content, Some(String::new()));

        // Finish returns the content
        let final_content = manager.finish_stream(&id).await;
        assert_eq!(final_content, Some(String::new()));

        // Stream should be gone
        assert!(manager.get_stream_content(&id).await.is_none());
    }

    #[tokio::test]
    async fn stream_manager_update_and_flush() {
        let manager = StreamManager::new(StreamConfig {
            edit_interval_ms: 0, // immediate flush
            buffer_threshold: 5,
            max_message_length: 4096,
        });

        let handle = manager.start_stream("telegram", "chat1").await;
        let id = handle.id.clone();

        // Small content: may not flush
        let should_flush = manager.update_stream(&id, "hi").await;
        assert!(should_flush.is_some());

        // After enough content, should flush
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
}