//! Real clarify backend: returns JSON signal for the agent loop to ask the user.

use async_trait::async_trait;
use serde_json::json;

use hermes_core::ToolError;
use crate::tools::clarify::ClarifyBackend;

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
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        // Return a structured JSON response that signals the agent loop
        // to pause and ask the user for input.
        let result = json!({
            "type": "clarify_request",
            "question": question,
            "choices": choices.unwrap_or(&[]),
            "awaiting_response": true,
        });
        Ok(result.to_string())
    }
}
