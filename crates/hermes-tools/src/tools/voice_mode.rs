//! Voice mode toggle.
//!
//! Maintains a process-wide atomic flag reflecting whether voice mode
//! (STT→Agent→TTS loop) is currently enabled. The actual STT/TTS loop is
//! run by the CLI / TUI layer which reads [`voice_mode_enabled`] on every
//! tick; this tool handler only flips the flag and returns the resulting
//! state so the model can confirm.
//!
//! Full local STT/TTS (`whisper-rs` + ONNX TTS) arrives in v1.1; for v1.0
//! the CLI uses an HTTP-only path (OpenAI Whisper + OpenAI TTS) keyed on
//! the flag this tool manages.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

static VOICE_MODE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Returns the current process-wide voice mode flag. The CLI voice loop
/// polls this every tick and activates / deactivates the mic accordingly.
pub fn voice_mode_enabled() -> bool {
    VOICE_MODE_ENABLED.load(Ordering::Relaxed)
}

/// Force the flag (for tests / CLI command-line --voice).
pub fn set_voice_mode_enabled(value: bool) {
    VOICE_MODE_ENABLED.store(value, Ordering::Relaxed);
}

pub struct VoiceModeHandler;

#[async_trait]
impl ToolHandler for VoiceModeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let previous = VOICE_MODE_ENABLED.load(Ordering::Relaxed);
        let requested = params.get("enabled").and_then(|v| v.as_bool());

        let new_state = match requested {
            Some(v) => v,
            // No argument → toggle.
            None => !previous,
        };

        VOICE_MODE_ENABLED.store(new_state, Ordering::Relaxed);

        Ok(json!({
            "voice_mode": new_state,
            "previous": previous,
            "status": if new_state { "enabled" } else { "disabled" },
            "note": if new_state {
                "Voice mode on: CLI will record mic input, transcribe via STT, and speak replies via TTS."
            } else {
                "Voice mode off: CLI will use text-only IO."
            }
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "enabled".into(),
            json!({
                "type":"boolean",
                "description":"Enable (true), disable (false), or omit to toggle voice mode"
            }),
        );
        tool_schema(
            "voice_mode",
            "Toggle voice mode. When enabled, the CLI loop records mic input, \
             transcribes via STT, and speaks assistant replies via TTS.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NB: the `VOICE_MODE_ENABLED` flag is process-wide, so we fold all
    // mutation checks into one sequential test to avoid racing against
    // other test threads.
    #[tokio::test]
    async fn handles_explicit_and_toggle() {
        let handler = VoiceModeHandler;

        set_voice_mode_enabled(false);

        let out = handler.execute(json!({"enabled": true})).await.unwrap();
        assert!(voice_mode_enabled());
        assert!(out.contains("\"voice_mode\":true"));

        let out = handler.execute(json!({"enabled": false})).await.unwrap();
        assert!(!voice_mode_enabled());
        assert!(out.contains("\"voice_mode\":false"));

        // Omitted → toggle (false → true, then true → false).
        handler.execute(json!({})).await.unwrap();
        assert!(voice_mode_enabled());
        handler.execute(json!({})).await.unwrap();
        assert!(!voice_mode_enabled());
    }
}
