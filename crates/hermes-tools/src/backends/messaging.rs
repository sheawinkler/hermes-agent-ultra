//! Real messaging backend: delegates to hermes-gateway for cross-platform sending.

use async_trait::async_trait;
use serde_json::json;

use hermes_core::ToolError;
use crate::tools::messaging::MessagingBackend;

/// Messaging backend that signals the gateway for cross-platform message sending.
/// The actual sending is handled by the hermes-gateway crate's platform adapters.
pub struct SignalMessagingBackend;

impl SignalMessagingBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SignalMessagingBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MessagingBackend for SignalMessagingBackend {
    async fn send(&self, platform: &str, recipient: &str, message: &str) -> Result<String, ToolError> {
        Ok(json!({
            "type": "messaging_request",
            "platform": platform,
            "recipient": recipient,
            "message": message,
            "status": "pending",
        }).to_string())
    }
}
