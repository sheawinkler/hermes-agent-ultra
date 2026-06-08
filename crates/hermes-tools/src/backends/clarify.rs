//! Real clarify backend: returns JSON signal for the agent loop to ask the user.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::clarify::ClarifyBackend;
use hermes_core::ToolError;
use tracing::debug;

/// Clarify backend that returns a JSON signal for the CLI/gateway layer
/// to present the question to the user. The actual UI interaction is
/// handled by the caller.
pub struct SignalClarifyBackend;

impl SignalClarifyBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SignalClarifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClarifyBackend for SignalClarifyBackend {
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
        _session_key: Option<&str>,
    ) -> Result<String, ToolError> {
        // Return a structured JSON response that signals the agent loop
        // to pause and ask the user for input.
        let result = json!({
            "type": "clarify_request",
            "question": question,
            "choices": choices.unwrap_or(&[]),
            "awaiting_response": true,
        });
        debug!(
            question = %question,
            choice_count = choices.map(|c| c.len()).unwrap_or(0),
            "signal clarify backend returning clarify_request (no channel UI wired)"
        );
        Ok(result.to_string())
    }
}
