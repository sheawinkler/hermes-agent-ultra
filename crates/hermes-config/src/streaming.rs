//! Streaming / progressive-output configuration.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// StreamingConfig
// ---------------------------------------------------------------------------

/// Configuration for streaming / progressive message output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamingConfig {
    /// Whether streaming is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Interval in milliseconds between edit operations when streaming.
    #[serde(default = "default_edit_interval_ms")]
    pub edit_interval_ms: u64,

    /// Number of buffered characters before forcing a flush.
    #[serde(default = "default_buffer_threshold")]
    pub buffer_threshold: usize,

    /// Maximum length of a single message before splitting.
    #[serde(default = "default_max_message_length")]
    pub max_message_length: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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

impl StreamingConfig {
    /// Round-trip serialize to serde_json::Value.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Round-trip deserialize from serde_json::Value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_config_default() {
        let sc = StreamingConfig::default();
        assert!(!sc.enabled);
        assert_eq!(sc.edit_interval_ms, 1000);
        assert_eq!(sc.buffer_threshold, 50);
        assert_eq!(sc.max_message_length, 4096);
    }

    #[test]
    fn streaming_config_roundtrip() {
        let sc = StreamingConfig {
            enabled: true,
            edit_interval_ms: 500,
            buffer_threshold: 100,
            max_message_length: 8192,
        };
        let value = sc.to_value();
        let back = StreamingConfig::from_value(value).unwrap();
        assert!(back.enabled);
        assert_eq!(back.edit_interval_ms, 500);
        assert_eq!(back.buffer_threshold, 100);
        assert_eq!(back.max_message_length, 8192);
    }

    #[test]
    fn streaming_config_partial_deserialize() {
        // Missing fields should use defaults
        let json = serde_json::json!({ "enabled": true });
        let sc: StreamingConfig = serde_json::from_value(json).unwrap();
        assert!(sc.enabled);
        assert_eq!(sc.edit_interval_ms, 1000);
        assert_eq!(sc.buffer_threshold, 50);
        assert_eq!(sc.max_message_length, 4096);
    }
}