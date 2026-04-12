//! Gateway hooks system.
//!
//! Hooks allow custom logic to run at specific points in the gateway lifecycle:
//! - pre_message: Before a message is sent to the agent
//! - post_message: After the agent responds
//! - on_error: When an error occurs
//! - on_session_start/end: Session lifecycle
//! - on_tool_call: Before/after tool execution

use hermes_core::Message;
use std::sync::Arc;

/// Hook event types.
#[derive(Debug, Clone)]
pub enum HookEvent {
    PreMessage {
        message: Message,
        session_id: String,
    },
    PostMessage {
        response: Message,
        session_id: String,
    },
    OnError {
        error: String,
        session_id: String,
    },
    SessionStart {
        session_id: String,
    },
    SessionEnd {
        session_id: String,
    },
    PreToolCall {
        tool_name: String,
        arguments: String,
        session_id: String,
    },
    PostToolCall {
        tool_name: String,
        result: String,
        session_id: String,
    },
}

/// Trait for gateway hook handlers.
#[async_trait::async_trait]
pub trait HookHandler: Send + Sync {
    async fn handle(&self, event: &HookEvent) -> Result<(), String>;
    fn name(&self) -> &str;
}

/// Gateway hooks manager.
pub struct HooksManager {
    handlers: Vec<Arc<dyn HookHandler>>,
}

impl HooksManager {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    pub fn register(&mut self, handler: Arc<dyn HookHandler>) {
        tracing::info!("Registered gateway hook: {}", handler.name());
        self.handlers.push(handler);
    }

    pub async fn emit(&self, event: &HookEvent) {
        for handler in &self.handlers {
            if let Err(e) = handler.handle(event).await {
                tracing::warn!("Hook '{}' error: {}", handler.name(), e);
            }
        }
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }
}

impl Default for HooksManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook;

    #[async_trait::async_trait]
    impl HookHandler for TestHook {
        async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
            Ok(())
        }
        fn name(&self) -> &str {
            "test_hook"
        }
    }

    #[test]
    fn test_register_hook() {
        let mut mgr = HooksManager::new();
        mgr.register(Arc::new(TestHook));
        assert_eq!(mgr.handler_count(), 1);
    }
}
