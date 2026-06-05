//! Messaging tool: send messages across platforms

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

/// Platform identifiers accepted by the Rust gateway-backed messaging tool.
pub const SUPPORTED_MESSAGING_PLATFORMS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "whatsapp",
    "signal",
    "email",
    "sms",
    "matrix",
    "mattermost",
    "dingtalk",
    "homeassistant",
    "feishu",
    "ntfy",
    "qqbot",
    "wecom",
    "wecom_callback",
    "webhook",
    "weixin",
    "bluebubbles",
];

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
        thread_id: Option<&str>,
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

        let thread_id = params
            .get("thread_id")
            .or_else(|| params.get("thread_ts"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        self.backend
            .send(platform, recipient, message, thread_id)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "platform".into(),
            json!({
                "type": "string",
                "description": "Platform to send the message on.",
                "enum": SUPPORTED_MESSAGING_PLATFORMS
            }),
        );
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
        props.insert(
            "thread_id".into(),
            json!({
                "type": "string",
                "description": "Optional platform-native thread id (for Slack this maps to thread_ts)"
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
            thread_id: Option<&str>,
        ) -> Result<String, ToolError> {
            let thread_suffix = thread_id
                .map(|thread| format!(" in thread {}", thread))
                .unwrap_or_default();
            Ok(format!(
                "Sent to {} on {}{}: {}",
                recipient, platform, thread_suffix, message
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

    #[tokio::test]
    async fn test_send_message_execute_accepts_thread_id_alias() {
        let handler = SendMessageHandler::new(Arc::new(MockMessagingBackend));
        let result = handler
            .execute(json!({
                "platform": "slack",
                "recipient": "C123",
                "message": "Hello!",
                "thread_ts": "171234.5"
            }))
            .await
            .unwrap();
        assert!(result.contains("slack"));
        assert!(result.contains("171234.5"));
    }
}
