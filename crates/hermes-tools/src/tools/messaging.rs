//! Messaging tool: send messages across platforms

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// MessagingBackend trait
// ---------------------------------------------------------------------------

/// Backend for sending messages across platforms.
#[async_trait]
pub trait MessagingBackend: Send + Sync {
    /// Send a message to a recipient on a platform.
    async fn send(
        &self,
        platform: &str,
        recipient: &str,
        message: &str,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// SendMessageHandler
// ---------------------------------------------------------------------------

/// Tool for sending messages across platforms.
pub struct SendMessageHandler {
    backend: Arc<dyn MessagingBackend>,
}

impl SendMessageHandler {
    pub fn new(backend: Arc<dyn MessagingBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for SendMessageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let platform = params
            .get("platform")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'platform' parameter".into()))?;

        let recipient = params
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'recipient' parameter".into()))?;

        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'message' parameter".into()))?;

        self.backend.send(platform, recipient, message).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("platform".into(), json!({
            "type": "string",
            "description": "Platform to send the message on (e.g. 'telegram', 'discord', 'slack')",
            "enum": ["telegram", "discord", "slack", "whatsapp", "signal", "email", "sms"]
        }));
        props.insert(
            "recipient".into(),
            json!({
                "type": "string",
                "description": "Recipient identifier (chat ID, user ID, email, phone number)"
            }),
        );
        props.insert(
            "message".into(),
            json!({
                "type": "string",
                "description": "Message content to send"
            }),
        );

        tool_schema(
            "send_message",
            "Send a message to a recipient on a specific platform.",
            JsonSchema::object(
                props,
                vec!["platform".into(), "recipient".into(), "message".into()],
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockMessagingBackend;
    #[async_trait]
    impl MessagingBackend for MockMessagingBackend {
        async fn send(
            &self,
            platform: &str,
            recipient: &str,
            message: &str,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Sent to {} on {}: {}",
                recipient, platform, message
            ))
        }
    }

    #[tokio::test]
    async fn test_send_message_schema() {
        let handler = SendMessageHandler::new(Arc::new(MockMessagingBackend));
        assert_eq!(handler.schema().name, "send_message");
    }

    #[tokio::test]
    async fn test_send_message_execute() {
        let handler = SendMessageHandler::new(Arc::new(MockMessagingBackend));
        let result = handler
            .execute(json!({
                "platform": "telegram",
                "recipient": "12345",
                "message": "Hello!"
            }))
            .await
            .unwrap();
        assert!(result.contains("telegram"));
        assert!(result.contains("12345"));
    }
}
